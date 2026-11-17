\set ON_ERROR_STOP on

-- Explicit disposable-fixture setup only. The production and native-test readers never execute this file.
-- The owner marker is supplied at startup by the existing nonce-owned container supervisor.
/* culprit:fixture */ DO $$
BEGIN
    IF current_database() <> 'culprit_timescale'
       OR pg_catalog.current_setting('cluster_name') !~ '^culprit_owned_[0-9a-f]{24}$'
       OR (SELECT extversion FROM pg_catalog.pg_extension WHERE extname='timescaledb') IS DISTINCT FROM '2.31.0'
       OR NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname='smoke_reader' AND NOT rolsuper)
       OR pg_catalog.to_regnamespace('culprit_jobs_test') IS NOT NULL THEN
        RAISE EXCEPTION 'A fresh, explicitly marked owned Timescale fixture is required';
    END IF;
END
$$;

/* culprit:fixture */ CREATE SCHEMA culprit_jobs_test;
/* culprit:fixture */ CREATE TABLE culprit_jobs_test.fixture_jobs (
    kind text PRIMARY KEY CHECK (kind IN ('admin','limited')),
    job_id integer NOT NULL UNIQUE,
    owner name NOT NULL
);
/* culprit:fixture */ CREATE PROCEDURE culprit_jobs_test.fail_native_job(job_id integer, config jsonb)
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION USING ERRCODE='22012', MESSAGE='culprit-jobs-private-error-canary';
END
$$;
/* culprit:fixture */ GRANT USAGE ON SCHEMA culprit_jobs_test TO smoke_reader;
/* culprit:fixture */ GRANT EXECUTE ON PROCEDURE culprit_jobs_test.fail_native_job(integer,jsonb) TO smoke_reader;
/* culprit:fixture */ GRANT SELECT,INSERT ON culprit_jobs_test.fixture_jobs TO smoke_reader;

/* culprit:fixture */ INSERT INTO culprit_jobs_test.fixture_jobs
SELECT 'admin',public.add_job('culprit_jobs_test.fail_native_job'::regproc,INTERVAL '1 day',
    config=>' {"private":"culprit-jobs-private-config-canary"}'::jsonb,scheduled=>false),current_user;
/* culprit:fixture */ SET ROLE smoke_reader;
/* culprit:fixture */ INSERT INTO culprit_jobs_test.fixture_jobs
SELECT 'limited',public.add_job('culprit_jobs_test.fail_native_job'::regproc,INTERVAL '1 day',
    config=>' {"private":"culprit-jobs-private-config-canary"}'::jsonb,scheduled=>false),current_user;
/* culprit:fixture */ RESET ROLE;
/* culprit:fixture */ REVOKE INSERT ON culprit_jobs_test.fixture_jobs FROM smoke_reader;

-- run_job() executes the procedure in the foreground; scheduler execution owns the retained run accounting.
-- https://github.com/timescale/timescaledb/blob/2.31.0/tsl/src/bgw_policy/job_api.c
-- https://github.com/timescale/timescaledb/blob/2.31.0/src/bgw/job.c
/* culprit:fixture */ SELECT altered.job_id FROM culprit_jobs_test.fixture_jobs fixture
CROSS JOIN LATERAL public.alter_job(fixture.job_id,scheduled=>true,next_start=>pg_catalog.clock_timestamp(),
    max_retries=>0,retry_period=>INTERVAL '1 day') altered;

-- Bounded setup polling is outside every measured source window. Nothing is written to native statistics tables.
/* culprit:fixture */ DO $$
DECLARE
    deadline timestamptz := pg_catalog.clock_timestamp() + INTERVAL '30 seconds';
BEGIN
    LOOP
        EXIT WHEN (
            SELECT count(*)=2 FROM culprit_jobs_test.fixture_jobs fixture
            JOIN timescaledb_information.job_stats stats USING(job_id)
            WHERE stats.total_runs>=1 AND stats.total_failures>=1
              AND EXISTS (
                  SELECT FROM timescaledb_information.job_history history
                  WHERE history.job_id=fixture.job_id AND history.succeeded=false
                    AND history.finish_time IS NOT NULL AND history.sqlerrcode='22012'
              )
        );
        IF pg_catalog.clock_timestamp() >= deadline THEN
            RAISE EXCEPTION 'The owned background job fixture did not settle before its deadline';
        END IF;
        PERFORM pg_catalog.pg_sleep(0.1);
    END LOOP;
END
$$;

/* culprit:fixture */ SELECT altered.job_id FROM culprit_jobs_test.fixture_jobs fixture
CROSS JOIN LATERAL public.alter_job(fixture.job_id,scheduled=>false) altered;
/* culprit:fixture */ DO $$
BEGIN
    IF (SELECT count(*) FROM culprit_jobs_test.fixture_jobs fixture
        JOIN timescaledb_information.jobs jobs USING(job_id)
        JOIN timescaledb_information.job_stats stats USING(job_id)
        WHERE NOT jobs.scheduled AND stats.total_runs=1 AND stats.total_failures=1 AND stats.total_successes=0) <> 2 THEN
        RAISE EXCEPTION 'The owned jobs must each retain exactly one settled failed execution';
    END IF;
END
$$;

-- Deliberate fixture-only permission difference. PUBLIC job_errors remains owner-filtered by the native view.
/* culprit:fixture */ REVOKE SELECT ON timescaledb_information.job_history FROM PUBLIC;
/* culprit:fixture */ SELECT kind,job_id,owner FROM culprit_jobs_test.fixture_jobs ORDER BY kind;
