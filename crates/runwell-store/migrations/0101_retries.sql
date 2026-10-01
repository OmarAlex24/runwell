CREATE TABLE retry_claims (
    repo TEXT NOT NULL,
    run_id INTEGER NOT NULL CHECK(run_id > 0),
    attempt INTEGER NOT NULL CHECK(attempt > 0),
    utc_day INTEGER NOT NULL,
    job_count INTEGER NOT NULL CHECK(job_count > 0),
    status TEXT NOT NULL CHECK(status IN ('claimed','accepted','rejected','ambiguous')),
    PRIMARY KEY(repo, run_id, attempt)
);
CREATE INDEX retry_daily_budget ON retry_claims(repo, utc_day);
CREATE TABLE retried_jobs (
    repo TEXT NOT NULL,
    run_id INTEGER NOT NULL,
    attempt INTEGER NOT NULL,
    github_job_id INTEGER NOT NULL CHECK(github_job_id > 0),
    PRIMARY KEY(repo, github_job_id, attempt),
    FOREIGN KEY(repo, run_id, attempt) REFERENCES retry_claims(repo, run_id, attempt)
);
