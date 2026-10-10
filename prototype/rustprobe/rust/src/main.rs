// PROTOTYPE, throwaway: Rust side of the Go-vs-Rust kernel benchmark.
// "verbatim" is a straight port of the Go verbatim variant: a fresh map per sample, internal iteration through
// closures, owned copies where Go keeps pointers. "tuned" mirrors the Go tuned variant line for line.
use std::collections::HashMap;
use std::hash::{BuildHasher, BuildHasherDefault, Hasher};
use std::io::{BufRead, BufWriter, Write};
use std::time::Instant;
use std::rc::Rc;

const WARMUP_MS: i64 = 5000;
const MIN_SAMPLES: usize = 20;
const TIE_V: f64 = 0.001;

fn request_for(core: usize) -> f32 {
    1.1 + 0.002 * (core % 8) as f32 + 0.0005 * (core / 8) as f32
}
fn clock_for(ccd: usize) -> i64 {
    5000 + 100 * ccd as i64
}

#[derive(Default, Clone)]
struct PmTable {
    _power_w: [f32; 16],
    voltage_request_v: [f32; 16],
    _temperature_c: [f32; 16],
    c0_pct: [f32; 16],
    _cc1_pct: [f32; 16],
    cc6_pct: [f32; 16],
}

struct Trial {
    ran_s: i64,
    stalled: i64,
    threads: i64,
    cores: Vec<usize>,
}

#[derive(Default)]
struct Out {
    last: i64,
    stalled: i64,
    stall_ms: i64,
    v: Option<(f64, f64)>,
    req: Vec<(usize, f64)>,
    top: Vec<usize>,
    mhz: Vec<(usize, i64)>,
}

impl Out {
    fn render(&self) -> String {
        use std::fmt::Write;
        let mut b = String::new();
        write!(b, "last={} stall={}@{}", self.last, self.stalled, self.stall_ms).unwrap();
        if let Some((m, n)) = self.v {
            write!(b, " v={:x}/{:x}", m.to_bits(), n.to_bits()).unwrap();
        }
        b.push_str(" req=");
        for (c, v) in &self.req {
            write!(b, "{}:{:x},", c, v.to_bits()).unwrap();
        }
        b.push_str(" top=");
        for c in &self.top {
            write!(b, "{},", c).unwrap();
        }
        b.push_str(" mhz=");
        for (c, m) in &self.mhz {
            write!(b, "{}:{},", c, m).unwrap();
        }
        b
    }
}

// ---- verbatim ----

#[derive(Default)]
struct FxHasher(u64);
impl Hasher for FxHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
    fn write_usize(&mut self, i: usize) {
        self.0 = (self.0.rotate_left(5) ^ i as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}
type Fx = BuildHasherDefault<FxHasher>;

struct TrialConditions<S> {
    elapsed_ms: i64,
    _tctl_c: Option<i64>,
    _tccd_c: Option<HashMap<String, i64, S>>,
    core_mhz: Option<Rc<HashMap<usize, i64, S>>>,
    worker_cpu_ms: HashMap<usize, i64, S>,
    _package_power_w: Option<f64>,
    pm_table: Option<Rc<PmTable>>,
}

struct TrialSamples {
    cores: Vec<usize>,
    threads: i64,
    ran_ms: i64,
    stalled_core: i64,
    requests: [f32; 16],
    clocks: [i64; 2],
}

impl TrialSamples {
    fn conditions<S: BuildHasher + Default>(&self, yield_: &mut dyn FnMut(TrialConditions<S>) -> bool) {
        let mut pm = PmTable { voltage_request_v: self.requests, ..Default::default() };
        let mut mhz: HashMap<usize, i64, S> = HashMap::with_capacity_and_hasher(self.cores.len(), S::default());
        for &core in &self.cores {
            mhz.insert(core, self.clocks[core / 8]);
            pm.c0_pct[core] = 100.0;
        }
        for core in 0..16 {
            pm.cc6_pct[core] = 100.0 - pm.c0_pct[core];
        }
        let (pm, mhz) = (Rc::new(pm), Rc::new(mhz));
        let mut at = 1000;
        while at < self.ran_ms {
            let mut cpu: HashMap<usize, i64, S> = HashMap::with_capacity_and_hasher(self.cores.len(), S::default());
            for &core in &self.cores {
                cpu.insert(core, self.worker_cpu_ms(core, at));
            }
            let sample = TrialConditions {
                elapsed_ms: at,
                _tctl_c: None,
                _tccd_c: None,
                core_mhz: Some(mhz.clone()),
                worker_cpu_ms: cpu,
                _package_power_w: None,
                pm_table: Some(pm.clone()),
            };
            if !yield_(sample) {
                return;
            }
            at += 1000;
        }
    }

    fn worker_cpu_ms(&self, core: usize, at: i64) -> i64 {
        let mut at = at;
        if core as i64 == self.stalled_core {
            at = at.min(1000.max(self.ran_ms - 2000));
        }
        at * self.threads
    }
}

#[derive(Default)]
struct Telemetry<S> {
    requests: HashMap<usize, f64, S>,
    top_requesters: Vec<usize>,
    ccd_mhz: Option<HashMap<usize, i64, S>>,
}

struct SampleSummary<S> {
    last_elapsed: Option<i64>,
    stalled_core: Option<usize>,
    worker_stalled_ms: Option<i64>,
    voltage: Option<(f64, f64)>,
    requests: Telemetry<S>,
}

fn sample_evidence<S: BuildHasher + Default>(
    samples: &dyn Fn(&mut dyn FnMut(TrialConditions<S>) -> bool),
    cores: &[usize],
    ccds: &HashMap<usize, usize, S>,
) -> SampleSummary<S> {
    #[derive(Clone, Copy, Default)]
    struct Worker {
        cpu: i64,
        stall: Option<i64>,
    }
    let mut workers = vec![Worker::default(); cores.len()];
    let mut last: Option<TrialConditions<S>> = None;
    let mut valid = cores.len() >= 2;
    let mut count = 0usize;
    let mut maxima: Vec<f64> = Vec::new();
    let mut observed = |consumer: &mut dyn FnMut(&TrialConditions<S>) -> bool| {
        let mut more = true;
        samples(&mut |sample| {
            requested_voltage_add(&mut maxima, &sample, cores);
            for (i, &core) in cores.iter().enumerate() {
                let (cpu, present) = match sample.worker_cpu_ms.get(&core) {
                    Some(&c) => (c, true),
                    None => (0, false),
                };
                if !present
                    || cpu < 0
                    || count > 0 && (cpu < workers[i].cpu || sample.elapsed_ms <= last.as_ref().unwrap().elapsed_ms)
                {
                    valid = false;
                }
                if count > 0 && cpu == workers[i].cpu {
                    if workers[i].stall.is_none() {
                        workers[i].stall = Some(sample.elapsed_ms);
                    }
                } else {
                    workers[i].stall = None;
                }
                workers[i].cpu = cpu;
            }
            count += 1;
            if more {
                more = consumer(&sample);
            }
            last = Some(sample);
            true
        });
    };
    let telemetry = if cores.is_empty() || cores.iter().any(|c| !ccds.contains_key(c)) {
        observed(&mut |_| true);
        Telemetry { requests: HashMap::default(), top_requesters: Vec::new(), ccd_mhz: None }
    } else {
        summarize(&mut observed, cores, |core| ccds[&core]).0
    };
    let voltage = median_minimum(&mut maxima);
    let mut summary = SampleSummary {
        last_elapsed: last.as_ref().map(|s| s.elapsed_ms),
        stalled_core: None,
        worker_stalled_ms: None,
        voltage,
        requests: telemetry,
    };
    if !valid || count < 2 {
        return summary;
    }
    let mut first: Option<usize> = None;
    let mut tie = false;
    for (i, w) in workers.iter().enumerate() {
        let Some(stall) = w.stall else { continue };
        match first {
            None => (first, tie) = (Some(i), false),
            Some(f) if stall < workers[f].stall.unwrap() => (first, tie) = (Some(i), false),
            Some(f) if stall == workers[f].stall.unwrap() => tie = true,
            _ => {}
        }
    }
    if let (Some(f), false) = (first, tie) {
        summary.stalled_core = Some(cores[f]);
        summary.worker_stalled_ms = workers[f].stall;
    }
    summary
}

fn requested_voltage_add<S>(maxima: &mut Vec<f64>, sample: &TrialConditions<S>, cores: &[usize]) {
    let Some(pm) = &sample.pm_table else { return };
    if cores.is_empty() {
        return;
    }
    let requests = &pm.voltage_request_v;
    let mut highest = 0f32;
    for (i, &core) in cores.iter().enumerate() {
        if core >= requests.len() {
            return;
        }
        if i == 0 || requests[core] > highest {
            highest = requests[core];
        }
    }
    maxima.push(highest as f64);
}

fn median_minimum(maxima: &mut [f64]) -> Option<(f64, f64)> {
    if maxima.is_empty() {
        return None;
    }
    maxima.sort_by(f64::total_cmp);
    let middle = maxima.len() / 2;
    let mut median = maxima[middle];
    if maxima.len() % 2 == 0 {
        median = (maxima[middle - 1] + median) / 2.0;
    }
    Some((median, maxima[0]))
}

fn summarize<S: BuildHasher + Default>(
    samples: &mut dyn FnMut(&mut dyn FnMut(&TrialConditions<S>) -> bool),
    cores: &[usize],
    ccd_of: impl Fn(usize) -> usize,
) -> (Telemetry<S>, bool) {
    let empty = || Telemetry { requests: HashMap::default(), top_requesters: Vec::new(), ccd_mhz: None };
    if cores.is_empty() {
        return (empty(), false);
    }
    let mut requests: HashMap<usize, Vec<f64>, S> = HashMap::with_capacity_and_hasher(cores.len(), S::default());
    let mut clocks: HashMap<usize, Vec<f64>, S> = HashMap::default();
    let mut per_ccd: HashMap<usize, Vec<f64>, S> = HashMap::default();
    let mut count = 0usize;
    let mut failed = false;
    samples(&mut |sample| {
        if sample.elapsed_ms < WARMUP_MS || sample.pm_table.is_none() {
            return true;
        }
        let lanes = &sample.pm_table.as_ref().unwrap().voltage_request_v;
        if cores.iter().any(|&c| c >= lanes.len()) {
            failed = true;
            return false;
        }
        for mhz in per_ccd.values_mut() {
            mhz.clear();
        }
        for &core in cores {
            requests.entry(core).or_default().push(lanes[core] as f64);
            if let Some(&mhz) = sample.core_mhz.as_ref().and_then(|m| m.get(&core)) {
                per_ccd.entry(ccd_of(core)).or_default().push(mhz as f64);
            }
        }
        for (ccd, mhz) in per_ccd.iter_mut() {
            if !mhz.is_empty() {
                clocks.entry(*ccd).or_default().push(median_in_place(mhz));
            }
        }
        count += 1;
        true
    });
    if failed || count < MIN_SAMPLES {
        return (empty(), false);
    }
    let mut t = Telemetry { requests: HashMap::with_capacity_and_hasher(cores.len(), S::default()), top_requesters: Vec::new(), ccd_mhz: None };
    let mut by_ccd: HashMap<usize, HashMap<usize, f64, S>, S> = HashMap::default();
    for (&core, values) in &requests {
        let m = median(values);
        t.requests.insert(core, m);
        by_ccd.entry(ccd_of(core)).or_default().insert(core, m);
    }
    for group in by_ccd.values() {
        t.top_requesters.extend_from_slice(&groups(group)[0]);
    }
    t.top_requesters.sort();
    for (&ccd, mhz) in &clocks {
        if mhz.len() < MIN_SAMPLES {
            continue;
        }
        t.ccd_mhz.get_or_insert_with(HashMap::default).insert(ccd, median(mhz).round() as i64);
    }
    (t, true)
}

fn groups<S: BuildHasher>(requests: &HashMap<usize, f64, S>) -> Vec<Vec<usize>> {
    let mut cores: Vec<usize> = requests.keys().copied().collect();
    cores.sort_by(|&a, &b| requests[&b].total_cmp(&requests[&a]).then(a.cmp(&b)));
    let mut groups = Vec::new();
    let mut rest = &cores[..];
    while !rest.is_empty() {
        let mut n = 1;
        while n < rest.len() && requests[&rest[n]] >= requests[&rest[0]] - TIE_V {
            n += 1;
        }
        let mut group = rest[..n].to_vec();
        group.sort();
        groups.push(group);
        rest = &rest[n..];
    }
    groups
}

fn median(values: &[f64]) -> f64 {
    median_in_place(&mut values.to_vec())
}

fn median_in_place(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        return (values[middle - 1] + values[middle]) / 2.0;
    }
    values[middle]
}

fn run_verbatim<S: BuildHasher + Default>(t: &Trial) -> String {
    let mut s = TrialSamples {
        cores: t.cores.clone(),
        threads: t.threads,
        ran_ms: t.ran_s * 1000,
        stalled_core: t.stalled,
        requests: [0.0; 16],
        clocks: [clock_for(0), clock_for(1)],
    };
    let mut ccds: HashMap<usize, usize, S> = HashMap::with_capacity_and_hasher(t.cores.len(), S::default());
    for core in 0..16 {
        s.requests[core] = request_for(core);
        ccds.insert(core, core / 8);
    }
    let sum = sample_evidence::<S>(&|y| s.conditions(y), &t.cores, &ccds);
    let mut o = Out { last: sum.last_elapsed.unwrap_or(-1), stalled: -1, stall_ms: -1, v: sum.voltage, ..Default::default() };
    if let Some(c) = sum.stalled_core {
        o.stalled = c as i64;
        o.stall_ms = sum.worker_stalled_ms.unwrap();
    }
    let mut keys: Vec<usize> = sum.requests.requests.keys().copied().collect();
    keys.sort();
    o.req = keys.iter().map(|c| (*c, sum.requests.requests[c])).collect();
    o.top = sum.requests.top_requesters;
    if let Some(m) = &sum.requests.ccd_mhz {
        let mut keys: Vec<usize> = m.keys().copied().collect();
        keys.sort();
        o.mhz = keys.iter().map(|c| (*c, m[c])).collect();
    }
    o.render()
}

// ---- tuned ----

#[derive(Default)]
struct Scratch {
    maxima: Vec<f64>,
    requests: [Vec<f64>; 16],
    clocks: [Vec<f64>; 2],
    per_ccd: [Vec<f64>; 2],
    tmp: Vec<f64>,
}

impl Scratch {
    fn median_of(&mut self, which: Which) -> f64 {
        self.tmp.clear();
        match which {
            Which::Request(c) => self.tmp.extend_from_slice(&self.requests[c]),
            Which::Clock(c) => self.tmp.extend_from_slice(&self.clocks[c]),
        }
        median_in_place(&mut self.tmp)
    }
}

enum Which {
    Request(usize),
    Clock(usize),
}

#[derive(Clone, Copy)]
struct TunedSample<'a> {
    elapsed_ms: i64,
    cpu: [i64; 16],
    present: u16,
    mhz: &'a [i64; 16],
    mhz_set: u16,
    pm: &'a PmTable,
}

fn run_tuned(t: &Trial, sc: &mut Scratch) -> String {
    sc.maxima.clear();
    sc.requests.iter_mut().for_each(Vec::clear);
    sc.clocks.iter_mut().for_each(Vec::clear);
    let ran = t.ran_s * 1000;
    let mut pm = PmTable::default();
    let mut mhz = [0i64; 16];
    let mut mhz_set = 0u16;
    for core in 0..16 {
        pm.voltage_request_v[core] = request_for(core);
    }
    for &core in &t.cores {
        mhz[core] = clock_for(core / 8);
        mhz_set |= 1 << core;
        pm.c0_pct[core] = 100.0;
    }
    for core in 0..16 {
        pm.cc6_pct[core] = 100.0 - pm.c0_pct[core];
    }
    #[derive(Clone, Copy, Default)]
    struct Worker {
        cpu: i64,
        stall: Option<i64>,
    }
    let mut workers = [Worker::default(); 16];
    let mut valid = t.cores.len() >= 2;
    let mut count = 0usize;
    let mut last_elapsed = 0i64;
    let telemetry_ok = !t.cores.is_empty();
    let mut req_count = 0usize;
    let mut at = 1000;
    while at < ran {
        let mut s = TunedSample { elapsed_ms: at, cpu: [0; 16], present: 0, mhz: &mhz, mhz_set, pm: &pm };
        for &core in &t.cores {
            let mut a = at;
            if core as i64 == t.stalled {
                a = a.min(1000.max(ran - 2000));
            }
            s.cpu[core] = a * t.threads;
            s.present |= 1 << core;
        }
        if !t.cores.is_empty() {
            let mut highest = 0f32;
            for (i, &core) in t.cores.iter().enumerate() {
                if i == 0 || s.pm.voltage_request_v[core] > highest {
                    highest = s.pm.voltage_request_v[core];
                }
            }
            sc.maxima.push(highest as f64);
        }
        for (i, &core) in t.cores.iter().enumerate() {
            let (cpu, present) = (s.cpu[core], s.present & (1 << core) != 0);
            if !present || cpu < 0 || count > 0 && (cpu < workers[i].cpu || s.elapsed_ms <= last_elapsed) {
                valid = false;
            }
            if count > 0 && cpu == workers[i].cpu {
                if workers[i].stall.is_none() {
                    workers[i].stall = Some(s.elapsed_ms);
                }
            } else {
                workers[i].stall = None;
            }
            workers[i].cpu = cpu;
        }
        last_elapsed = s.elapsed_ms;
        count += 1;
        if telemetry_ok && s.elapsed_ms >= WARMUP_MS {
            sc.per_ccd[0].clear();
            sc.per_ccd[1].clear();
            for &core in &t.cores {
                sc.requests[core].push(s.pm.voltage_request_v[core] as f64);
                if s.mhz_set & (1 << core) != 0 {
                    sc.per_ccd[core / 8].push(s.mhz[core] as f64);
                }
            }
            for ccd in 0..2 {
                if !sc.per_ccd[ccd].is_empty() {
                    let m = median_in_place(&mut sc.per_ccd[ccd]);
                    sc.clocks[ccd].push(m);
                }
            }
            req_count += 1;
        }
        at += 1000;
    }
    let mut o = Out { last: if count > 0 { last_elapsed } else { -1 }, stalled: -1, stall_ms: -1, ..Default::default() };
    if !sc.maxima.is_empty() {
        o.v = median_minimum(&mut sc.maxima);
    }
    if telemetry_ok && req_count >= MIN_SAMPLES {
        let mut reqs = [0f64; 16];
        let mut has = 0u16;
        for core in 0..16 {
            if !sc.requests[core].is_empty() {
                reqs[core] = sc.median_of(Which::Request(core));
                has |= 1 << core;
                o.req.push((core, reqs[core]));
            }
        }
        for ccd in 0..2 {
            let mut best: Option<usize> = None;
            for core in ccd * 8..ccd * 8 + 8 {
                if has & (1 << core) == 0 {
                    continue;
                }
                if best.is_none_or(|b| reqs[core] > reqs[b]) {
                    best = Some(core);
                }
            }
            let Some(best) = best else { continue };
            for core in ccd * 8..ccd * 8 + 8 {
                if has & (1 << core) != 0 && reqs[core] >= reqs[best] - TIE_V {
                    o.top.push(core);
                }
            }
        }
        o.top.sort();
        for ccd in 0..2 {
            if sc.clocks[ccd].len() >= MIN_SAMPLES {
                o.mhz.push((ccd, sc.median_of(Which::Clock(ccd)).round() as i64));
            }
        }
    }
    if valid && count >= 2 {
        let mut first: Option<usize> = None;
        let mut tie = false;
        for i in 0..t.cores.len() {
            let Some(stall) = workers[i].stall else { continue };
            match first {
                None => (first, tie) = (Some(i), false),
                Some(f) if stall < workers[f].stall.unwrap() => (first, tie) = (Some(i), false),
                Some(f) if stall == workers[f].stall.unwrap() => tie = true,
                _ => {}
            }
        }
        if let (Some(f), false) = (first, tie) {
            o.stalled = t.cores[f] as i64;
            o.stall_ms = workers[f].stall.unwrap();
        }
    }
    o.render()
}

fn load(path: &str) -> Vec<Trial> {
    let f = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    f.lines()
        .map(|l| {
            let l = l.unwrap();
            let p: Vec<&str> = l.split('\t').collect();
            Trial {
                ran_s: p[0].parse().unwrap(),
                stalled: p[1].parse().unwrap(),
                threads: p[2].parse().unwrap(),
                cores: if p.len() > 3 && !p[3].is_empty() { p[3].split(',').map(|c| c.parse().unwrap()).collect() } else { Vec::new() },
            }
        })
        .collect()
}

fn main() {
    let mut variant = "verbatim".to_string();
    let mut input = "trials.tsv".to_string();
    let mut reps = 1usize;
    let mut output: Option<String> = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let v = args[i + 1].clone();
        match args[i].as_str() {
            "-variant" => variant = v,
            "-in" => input = v,
            "-reps" => reps = v.parse().unwrap(),
            "-out" => output = Some(v),
            a => panic!("unknown flag {a}"),
        }
        i += 2;
    }
    let ts = load(&input);
    let mut w = output.map(|p| BufWriter::new(std::fs::File::create(p).unwrap()));
    let mut sc = Scratch::default();
    let start = Instant::now();
    let mut h: u64 = 14695981039346656037;
    for rep in 0..reps {
        for t in &ts {
            let line = match variant.as_str() {
                "tuned" => run_tuned(t, &mut sc),
                "verbatim-fx" => run_verbatim::<Fx>(t),
                _ => run_verbatim::<std::collections::hash_map::RandomState>(t),
            };
            for b in line.bytes() {
                h = (h ^ b as u64).wrapping_mul(1099511628211);
            }
            if rep == 0 {
                if let Some(w) = w.as_mut() {
                    w.write_all(line.as_bytes()).unwrap();
                    w.write_all(b"\n").unwrap();
                }
            }
        }
    }
    eprintln!(
        "rust-{} trials={} reps={} kernel={:.3}s hash={:016x}",
        variant,
        ts.len(),
        reps,
        start.elapsed().as_secs_f64(),
        h
    );
}
