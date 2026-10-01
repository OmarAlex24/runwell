CREATE TABLE completed_jobs (
    repo TEXT NOT NULL,
    github_job_id INTEGER NOT NULL CHECK(github_job_id > 0),
    run_id INTEGER NOT NULL CHECK(run_id > 0),
    attempt INTEGER NOT NULL CHECK(attempt > 0),
    workflow_job TEXT NOT NULL,
    class TEXT NOT NULL,
    completed_at INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL CHECK(duration_ms >= 0),
    queue_ms INTEGER NOT NULL CHECK(queue_ms >= 0),
    conclusion TEXT NOT NULL,
    depth INTEGER NOT NULL,
    fan_out INTEGER NOT NULL,
    PRIMARY KEY(repo, github_job_id, attempt)
);
CREATE INDEX completed_history_window ON completed_jobs
    (repo, workflow_job, class, conclusion, completed_at DESC, github_job_id DESC);
CREATE TABLE duration_estimates (
    repo TEXT NOT NULL,
    workflow_job TEXT NOT NULL,
    class TEXT NOT NULL,
    p50_seconds REAL NOT NULL,
    p90_seconds REAL NOT NULL,
    samples INTEGER NOT NULL,
    depth INTEGER NOT NULL,
    fan_out INTEGER NOT NULL,
    PRIMARY KEY(repo, workflow_job, class)
);
