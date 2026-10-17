-- Checks of views.sql against testdata/live, as test.sh prepares it: trial 0001 has plain samples and a
-- doubled gzip copy, trial 0002 only gzip samples, and the archived session reuses trial ID 0001.
.mode list
.headers off

create macro expect(ok, what) as if(ok, 'ok ' || what, error('failed: ' || what));

select expect(
    (select list(session || ' ' || archived || ' ' || seq order by session, seq) from events)
        = ['20260101T000000Z true 1', '20260101T000000Z true 2', '20260101T000000Z true 3',
           '20260102T000000Z false 1', '20260102T000000Z false 2', '20260102T000000Z false 3',
           '20260102T000000Z false 4'],
    'events of both journals carry their session, without the torn tail'
);

select expect(
    (select list(session || ' ' || trial || ' ' || cores::VARCHAR || ' ' || coalesce(outcome, 'running') order by session, trial) from trials)
        = ['20260101T000000Z 0001 [5] failure', '20260102T000000Z 0001 [3] pass', '20260102T000000Z 0002 [0, 1] running'],
    'trials join intent and end within a session'
);

select expect(
    (select list(session || ' ' || trial || ' ' || elapsed_ms order by session, trial, elapsed_ms) from samples)
        = ['20260101T000000Z 0001 1000', '20260101T000000Z 0001 2000', '20260101T000000Z 0001 3000',
           '20260102T000000Z 0001 1000', '20260102T000000Z 0001 2000', '20260102T000000Z 0002 1000'],
    'samples read plain and gzip files, preferring the plain one, in both sessions'
);

select expect(
    (select list(session || ' ' || trial || ' ' || core || ' ' || "offset" || ' ' || coalesce(mhz::VARCHAR, '-') || ' ' || coalesce(voltage_request_v::VARCHAR, '-')
        order by session, trial, elapsed_ms, core) from core_samples)
        = ['20260101T000000Z 0001 5 -20 5300 -', '20260101T000000Z 0001 5 -20 5300 -', '20260101T000000Z 0001 5 -20 - -',
           '20260102T000000Z 0001 3 -10 5000 1.2', '20260102T000000Z 0001 3 -10 5100 1.25',
           '20260102T000000Z 0002 0 -5 4800 -', '20260102T000000Z 0002 1 -7 4900 -'],
    'core samples join each loaded core to its offset in its own session'
);
