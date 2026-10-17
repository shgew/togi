-- Views over a togi state directory, read by sql.sh after it sets `state_dir` (absolute) and `empty`
-- (an empty JSONL file). The file lists are taken once, when DuckDB starts: restart it to see trials
-- and archives written since. Malformed lines, such as a torn tail, are skipped, so `togi events` and
-- `just stats` stay what a review reports from.

set TimeZone = 'UTC';

-- read_json refuses an empty file list, so every list carries the empty file, which reads as no rows.
set variable live_journal = getvariable('state_dir') || '/events.jsonl';

set variable journals = [getvariable('empty')] || coalesce((
    select list(file order by file)
    from (
        from glob(getvariable('live_journal'))
        union all
        from glob(getvariable('state_dir') || '/archive/*.jsonl')
    )
), []);

-- A trial's samples are plain while it runs and gzip-compressed once it ended; when a crash left both,
-- the plain file is the one togi reads, so the compressed one is skipped.
set variable sample_files = [getvariable('empty')] || coalesce((
    with found as (
        from glob(getvariable('state_dir') || '/trials/*/samples.jsonl')
        union all
        from glob(getvariable('state_dir') || '/trials/*/samples.jsonl.gz')
        union all
        from glob(getvariable('state_dir') || '/archive/*-trials/*/samples.jsonl')
        union all
        from glob(getvariable('state_dir') || '/archive/*-trials/*/samples.jsonl.gz')
    )
    select list(file order by file)
    from found
    where not (file like '%.gz' and file[:-4] in (select file from found))
), []);

set variable live_session = (
    select data ->> 'session'
    from read_json(
        getvariable('journals'),
        format = 'newline_delimited',
        columns = {kind: 'VARCHAR', data: 'JSON'},
        filename = true,
        ignore_errors = true
    )
    where filename = getvariable('live_journal') and kind = 'session.start'
    limit 1
);

-- Every event of the current journal and of the archived ones, with the session it belongs to.
create view events as
select
    if(filename = getvariable('live_journal'), getvariable('live_session'), parse_filename(filename, true)) as session,
    filename <> getvariable('live_journal') as archived,
    seq,
    time,
    boot,
    mono_ms,
    cause,
    kind,
    msg,
    data
from read_json(
    getvariable('journals'),
    format = 'newline_delimited',
    columns = {
        seq: 'BIGINT',
        time: 'TIMESTAMPTZ',
        boot: 'VARCHAR',
        mono_ms: 'BIGINT',
        cause: 'BIGINT[]',
        kind: 'VARCHAR',
        msg: 'VARCHAR',
        data: 'JSON'
    },
    filename = true,
    ignore_errors = true
)
where seq is not null;

-- One row per trial: its `trial.intent` and, once it ended, its `trial.end`. Trial IDs restart in every
-- session, so (session, trial) is the key. `cores` lists the loaded cores the intent names; `profile`
-- maps every core to its offset during the trial.
create view trials as
select
    session,
    any_value(archived) as archived,
    data ->> 'trial' as trial,
    min(time) filter (kind = 'trial.intent') as started,
    min(time) filter (kind = 'trial.end') as ended,
    any_value(data ->> 'regime') filter (kind = 'trial.intent') as regime,
    any_value(data ->> 'workload') filter (kind = 'trial.intent') as workload,
    any_value(data ->> 'condition') filter (kind = 'trial.intent') as condition,
    any_value(data ->> 'activity') filter (kind = 'trial.intent') as activity,
    any_value(coalesce(cast(data -> 'cores' as INTEGER[]), [cast(data ->> 'core' as INTEGER)]))
        filter (kind = 'trial.intent') as cores,
    any_value(cast(data -> 'profile' as MAP(INTEGER, INTEGER))) filter (kind = 'trial.intent') as profile,
    any_value(data ->> 'outcome') filter (kind = 'trial.end') as outcome,
    any_value(cast(data ->> 'duration_s' as BIGINT)) filter (kind = 'trial.end') as duration_s,
    any_value(cast(data ->> 'tctl_max_c' as BIGINT)) filter (kind = 'trial.end') as tctl_max_c,
    any_value(data) filter (kind = 'trial.intent') as intent_data,
    any_value(data) filter (kind = 'trial.end') as end_data
from events
where kind in ('trial.intent', 'trial.end')
group by session, trial;

-- Every per-second conditions sample of every trial, current and archived (journal.md, Files).
create view samples as
select
    if(
        starts_with(filename, getvariable('state_dir') || '/archive/'),
        regexp_extract(filename, '/archive/([^/]+)-trials/[^/]+/samples\.jsonl(\.gz)?$', 1),
        getvariable('live_session')
    ) as session,
    starts_with(filename, getvariable('state_dir') || '/archive/') as archived,
    regexp_extract(filename, '/([^/]+)/samples\.jsonl(\.gz)?$', 1) as trial,
    elapsed_ms,
    tctl_c,
    tccd_c,
    core_mhz,
    worker_cpu_ms,
    package_power_w,
    pm_table
from read_json(
    getvariable('sample_files'),
    format = 'newline_delimited',
    columns = {
        elapsed_ms: 'BIGINT',
        tctl_c: 'BIGINT',
        tccd_c: 'MAP(VARCHAR, BIGINT)',
        core_mhz: 'MAP(INTEGER, BIGINT)',
        worker_cpu_ms: 'MAP(INTEGER, BIGINT)',
        package_power_w: 'DOUBLE',
        pm_table: 'STRUCT(power_w DOUBLE[], voltage_request_v DOUBLE[], temperature_c DOUBLE[], c0_pct DOUBLE[], cc1_pct DOUBLE[], cc6_pct DOUBLE[])'
    },
    filename = true,
    ignore_errors = true
)
where elapsed_ms is not null;

-- One row per sample and loaded core (a core with a clock or a worker in that sample), with the core's
-- offset from the trial's profile and its own lanes of the decoded PM table.
create view core_samples as
select
    s.session,
    s.archived,
    s.trial,
    t.regime,
    t.workload,
    core,
    t.profile[core] as "offset",
    s.elapsed_ms,
    s.core_mhz[core] as mhz,
    s.worker_cpu_ms[core] as worker_cpu_ms,
    s.pm_table.voltage_request_v[core + 1] as voltage_request_v,
    s.pm_table.power_w[core + 1] as power_w,
    s.pm_table.temperature_c[core + 1] as temperature_c,
    s.pm_table.c0_pct[core + 1] as c0_pct,
    s.pm_table.cc1_pct[core + 1] as cc1_pct,
    s.pm_table.cc6_pct[core + 1] as cc6_pct,
    s.tctl_c,
    s.package_power_w
from samples s
left join trials t on t.session = s.session and t.trial = s.trial
cross join unnest(list_sort(list_distinct(list_concat(map_keys(s.core_mhz), map_keys(s.worker_cpu_ms))))) as loaded(core);
