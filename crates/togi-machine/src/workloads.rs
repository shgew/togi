use crate::{Backend, DutyPct, REGIMES, Regime};

const fn duty(pct: u8) -> DutyPct {
    match DutyPct::new(pct) {
        Some(pct) => pct,
        None => panic!("duty percentage above 100"),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workload {
    pub id: &'static str,
    pub base: &'static str,
    pub backend: Backend,
    pub label: &'static str,
    pub threads: u8,
    pub duty_pct: DutyPct,
    key: WorkloadId,
}

impl Workload {
    pub const fn id(&self) -> WorkloadId {
        self.key
    }
}

/// A workload of the catalog, named by its regime and its place in that regime's list. It orders the way the
/// catalog lists workloads and serializes as the catalog id string; an id outside the catalog does not decode.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkloadId {
    regime: Regime,
    index: u8,
}

impl WorkloadId {
    const fn catalog(regime: Regime, index: u8) -> Self {
        Self { regime, index }
    }

    pub fn new(regime: Regime, index: usize) -> Option<Self> {
        if index >= workloads(regime).len() {
            return None;
        }
        Some(Self::catalog(regime, u8::try_from(index).ok()?))
    }

    pub fn by_id(id: &str) -> Option<Self> {
        REGIMES.into_iter().find_map(|regime| {
            let index = workloads(regime).iter().position(|workload| workload.id == id)?;
            Self::new(regime, index)
        })
    }

    pub const fn regime(self) -> Regime {
        self.regime
    }

    pub fn workload(self) -> &'static Workload {
        &workloads(self.regime)[usize::from(self.index)]
    }

    pub fn as_str(self) -> &'static str {
        self.workload().id
    }

    pub fn backend(self) -> Backend {
        self.workload().backend
    }
}

impl std::fmt::Display for WorkloadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("unknown workload {0:?}")]
pub struct UnknownWorkload(pub String);

impl std::str::FromStr for WorkloadId {
    type Err = UnknownWorkload;

    fn from_str(id: &str) -> Result<Self, UnknownWorkload> {
        Self::by_id(id).ok_or_else(|| UnknownWorkload(id.to_owned()))
    }
}

impl serde::Serialize for WorkloadId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for WorkloadId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = std::borrow::Cow::<str>::deserialize(deserializer)?;
        id.parse().map_err(serde::de::Error::custom)
    }
}

pub const MPRIME_AVX512_ID: &str = "mprime-avx512-36k-248k";

static R1: [Workload; 3] = [
    Workload {
        id: "mprime-sse-4k-21k",
        base: "mprime-sse-4k-21k",
        backend: Backend::Mprime,
        label: "mprime SSE 4K-21K",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R1, 0),
    },
    Workload {
        id: "ycruncher-bkt-sftv4",
        base: "ycruncher-bkt-sftv4",
        backend: Backend::Ycruncher,
        label: "y-cruncher BKT + SFTv4",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R1, 1),
    },
    Workload {
        id: "ycruncher-snt-svt",
        base: "ycruncher-snt-svt",
        backend: Backend::Ycruncher,
        label: "y-cruncher SNT + SVT",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R1, 2),
    },
];

static R2: [Workload; 3] = [
    Workload {
        id: "mprime-avx2-36k-248k",
        base: "mprime-avx2-36k-248k",
        backend: Backend::Mprime,
        label: "mprime AVX2 36K-248K",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R2, 0),
    },
    Workload {
        id: "mprime-avx512-36k-248k",
        base: "mprime-avx512-36k-248k",
        backend: Backend::Mprime,
        label: "mprime AVX-512 36K-248K",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R2, 1),
    },
    Workload {
        id: "ycruncher-fftv4-n63-vt3",
        base: "ycruncher-fftv4-n63-vt3",
        backend: Backend::Ycruncher,
        label: "y-cruncher FFTv4 + N63 + VT3",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R2, 2),
    },
];

static R3: [Workload; 6] = [
    Workload {
        id: "mprime-sse-4k-21k-steps",
        base: "mprime-sse-4k-21k",
        backend: Backend::Mprime,
        label: "mprime SSE 4K-21K load steps",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R3, 0),
    },
    Workload {
        id: "ycruncher-bkt-sftv4-steps",
        base: "ycruncher-bkt-sftv4",
        backend: Backend::Ycruncher,
        label: "y-cruncher BKT + SFTv4 load steps",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R3, 1),
    },
    Workload {
        id: "ycruncher-snt-svt-steps",
        base: "ycruncher-snt-svt",
        backend: Backend::Ycruncher,
        label: "y-cruncher SNT + SVT load steps",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R3, 2),
    },
    Workload {
        id: "mprime-avx2-36k-248k-steps",
        base: "mprime-avx2-36k-248k",
        backend: Backend::Mprime,
        label: "mprime AVX2 36K-248K load steps",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R3, 3),
    },
    Workload {
        id: "mprime-avx512-36k-248k-steps",
        base: "mprime-avx512-36k-248k",
        backend: Backend::Mprime,
        label: "mprime AVX-512 36K-248K load steps",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R3, 4),
    },
    Workload {
        id: "ycruncher-fftv4-n63-vt3-steps",
        base: "ycruncher-fftv4-n63-vt3",
        backend: Backend::Ycruncher,
        label: "y-cruncher FFTv4 + N63 + VT3 load steps",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R3, 5),
    },
];

static R4: [Workload; 9] = [
    Workload {
        id: "mprime-sse-4k-21k-duty25",
        base: "mprime-sse-4k-21k",
        backend: Backend::Mprime,
        label: "mprime SSE 4K-21K 25% duty",
        threads: 1,
        duty_pct: duty(25),
        key: WorkloadId::catalog(Regime::R4, 0),
    },
    Workload {
        id: "ycruncher-bkt-sftv4-duty50",
        base: "ycruncher-bkt-sftv4",
        backend: Backend::Ycruncher,
        label: "y-cruncher BKT + SFTv4 50% duty",
        threads: 1,
        duty_pct: duty(50),
        key: WorkloadId::catalog(Regime::R4, 1),
    },
    Workload {
        id: "ycruncher-snt-svt-duty75",
        base: "ycruncher-snt-svt",
        backend: Backend::Ycruncher,
        label: "y-cruncher SNT + SVT 75% duty",
        threads: 1,
        duty_pct: duty(75),
        key: WorkloadId::catalog(Regime::R4, 2),
    },
    Workload {
        id: "mprime-sse-4k-21k-duty50",
        base: "mprime-sse-4k-21k",
        backend: Backend::Mprime,
        label: "mprime SSE 4K-21K 50% duty",
        threads: 1,
        duty_pct: duty(50),
        key: WorkloadId::catalog(Regime::R4, 3),
    },
    Workload {
        id: "ycruncher-bkt-sftv4-duty75",
        base: "ycruncher-bkt-sftv4",
        backend: Backend::Ycruncher,
        label: "y-cruncher BKT + SFTv4 75% duty",
        threads: 1,
        duty_pct: duty(75),
        key: WorkloadId::catalog(Regime::R4, 4),
    },
    Workload {
        id: "ycruncher-snt-svt-duty25",
        base: "ycruncher-snt-svt",
        backend: Backend::Ycruncher,
        label: "y-cruncher SNT + SVT 25% duty",
        threads: 1,
        duty_pct: duty(25),
        key: WorkloadId::catalog(Regime::R4, 5),
    },
    Workload {
        id: "mprime-sse-4k-21k-duty75",
        base: "mprime-sse-4k-21k",
        backend: Backend::Mprime,
        label: "mprime SSE 4K-21K 75% duty",
        threads: 1,
        duty_pct: duty(75),
        key: WorkloadId::catalog(Regime::R4, 6),
    },
    Workload {
        id: "ycruncher-bkt-sftv4-duty25",
        base: "ycruncher-bkt-sftv4",
        backend: Backend::Ycruncher,
        label: "y-cruncher BKT + SFTv4 25% duty",
        threads: 1,
        duty_pct: duty(25),
        key: WorkloadId::catalog(Regime::R4, 7),
    },
    Workload {
        id: "ycruncher-snt-svt-duty50",
        base: "ycruncher-snt-svt",
        backend: Backend::Ycruncher,
        label: "y-cruncher SNT + SVT 50% duty",
        threads: 1,
        duty_pct: duty(50),
        key: WorkloadId::catalog(Regime::R4, 8),
    },
];

static R5: [Workload; 6] = [
    Workload {
        id: "mprime-sse-4k-21k-smt",
        base: "mprime-sse-4k-21k",
        backend: Backend::Mprime,
        label: "mprime SSE 4K-21K SMT pair",
        threads: 2,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R5, 0),
    },
    Workload {
        id: "ycruncher-bkt-sftv4-smt",
        base: "ycruncher-bkt-sftv4",
        backend: Backend::Ycruncher,
        label: "y-cruncher BKT + SFTv4 SMT pair",
        threads: 2,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R5, 1),
    },
    Workload {
        id: "ycruncher-snt-svt-smt",
        base: "ycruncher-snt-svt",
        backend: Backend::Ycruncher,
        label: "y-cruncher SNT + SVT SMT pair",
        threads: 2,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R5, 2),
    },
    Workload {
        id: "mprime-avx2-36k-248k-smt",
        base: "mprime-avx2-36k-248k",
        backend: Backend::Mprime,
        label: "mprime AVX2 36K-248K SMT pair",
        threads: 2,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R5, 3),
    },
    Workload {
        id: "mprime-avx512-36k-248k-smt",
        base: "mprime-avx512-36k-248k",
        backend: Backend::Mprime,
        label: "mprime AVX-512 36K-248K SMT pair",
        threads: 2,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R5, 4),
    },
    Workload {
        id: "ycruncher-fftv4-n63-vt3-smt",
        base: "ycruncher-fftv4-n63-vt3",
        backend: Backend::Ycruncher,
        label: "y-cruncher FFTv4 + N63 + VT3 SMT pair",
        threads: 2,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R5, 5),
    },
];

static R6: [Workload; 3] = [
    Workload {
        id: "mprime-sse-4k-21k-idle",
        base: "mprime-sse-4k-21k",
        backend: Backend::Mprime,
        label: "mprime SSE 4K-21K idle then bursts",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R6, 0),
    },
    Workload {
        id: "ycruncher-bkt-sftv4-idle",
        base: "ycruncher-bkt-sftv4",
        backend: Backend::Ycruncher,
        label: "y-cruncher BKT + SFTv4 idle then bursts",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R6, 1),
    },
    Workload {
        id: "ycruncher-snt-svt-idle",
        base: "ycruncher-snt-svt",
        backend: Backend::Ycruncher,
        label: "y-cruncher SNT + SVT idle then bursts",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R6, 2),
    },
];

static R7: [Workload; 3] = [
    Workload {
        id: "mprime-avx2-36k-248k-allcore",
        base: "mprime-avx2-36k-248k",
        backend: Backend::Mprime,
        label: "mprime AVX2 36K-248K all-core",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R7, 0),
    },
    Workload {
        id: "mprime-avx512-36k-248k-allcore",
        base: "mprime-avx512-36k-248k",
        backend: Backend::Mprime,
        label: "mprime AVX-512 36K-248K all-core",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R7, 1),
    },
    Workload {
        id: "ycruncher-fftv4-n63-vt3-allcore",
        base: "ycruncher-fftv4-n63-vt3",
        backend: Backend::Ycruncher,
        label: "y-cruncher FFTv4 + N63 + VT3 all-core",
        threads: 1,
        duty_pct: duty(0),
        key: WorkloadId::catalog(Regime::R7, 2),
    },
];

pub fn workloads(regime: Regime) -> &'static [Workload] {
    match regime {
        Regime::R1 => &R1,
        Regime::R2 => &R2,
        Regime::R3 => &R3,
        Regime::R4 => &R4,
        Regime::R5 => &R5,
        Regime::R6 => &R6,
        Regime::R7 => &R7,
    }
}

pub fn pick_workload(regime: Regime, index: usize) -> &'static Workload {
    let catalog = workloads(regime);
    &catalog[index % catalog.len()]
}

pub fn workload_by_id(id: &str) -> Option<&'static Workload> {
    [
        &R1[..],
        &R2[..],
        &R3[..],
        &R4[..],
        &R5[..],
        &R6[..],
        &R7[..],
    ]
    .into_iter()
    .flatten()
    .find(|workload| workload.id == id)
}
