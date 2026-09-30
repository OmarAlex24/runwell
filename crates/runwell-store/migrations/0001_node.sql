CREATE TABLE jobs (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 scale_set_id INTEGER NOT NULL,
 request_id INTEGER NOT NULL,
 github_job_id TEXT NOT NULL,
 workflow_run_id INTEGER NOT NULL DEFAULT 0,
 repo TEXT NOT NULL,
 name TEXT NOT NULL,
 class TEXT NOT NULL,
 state TEXT NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','admitted','runner_created','running','completed','failed','orphaned')),
 acquired INTEGER NOT NULL DEFAULT 0,
 actual_request_id INTEGER,
 outcome TEXT,
 outcome_at INTEGER,
 reserved_cpu INTEGER NOT NULL,
 reserved_memory INTEGER NOT NULL,
 created_at INTEGER NOT NULL DEFAULT (unixepoch()),
 updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
 started_at INTEGER,
 finished_at INTEGER,
 UNIQUE(scale_set_id, request_id)
);
CREATE TABLE runners (
 job_id INTEGER PRIMARY KEY REFERENCES jobs(id),
 name TEXT NOT NULL UNIQUE,
 dir TEXT NOT NULL UNIQUE,
 unit TEXT NOT NULL UNIQUE,
 template_version TEXT NOT NULL,
 agent_id INTEGER UNIQUE,
 exit_code INTEGER,
 remote_deleted INTEGER NOT NULL DEFAULT 0,
 cleaned INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE measurements (
 job_id INTEGER PRIMARY KEY REFERENCES jobs(id),
 cpu_usec INTEGER NOT NULL,
 memory_current INTEGER NOT NULL,
 memory_peak INTEGER NOT NULL,
 io_read_bytes INTEGER NOT NULL,
 io_write_bytes INTEGER NOT NULL,
 psi TEXT NOT NULL,
 oom_kills INTEGER NOT NULL,
 duration_ms INTEGER NOT NULL,
 exit_code INTEGER,
 infra_signal INTEGER NOT NULL,
 recorded_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE TABLE messages (
 scale_set_id INTEGER PRIMARY KEY,
 last_acked_id INTEGER NOT NULL
);
CREATE TRIGGER legal_job_transition BEFORE UPDATE OF state ON jobs
WHEN OLD.state != NEW.state AND NOT (
 (OLD.state = 'queued' AND NEW.state = 'admitted') OR
 (OLD.state = 'admitted' AND NEW.state = 'runner_created') OR
 (OLD.state = 'runner_created' AND NEW.state = 'running') OR
 (OLD.state = 'running' AND NEW.state IN ('completed','failed','orphaned')) OR
 (OLD.state IN ('queued','admitted','runner_created') AND NEW.state IN ('failed','orphaned'))
)
BEGIN SELECT RAISE(ABORT, 'illegal job state transition'); END;
CREATE TRIGGER registered_before_execution BEFORE UPDATE OF state ON jobs
WHEN NEW.state IN ('runner_created','running') AND NOT (
 NEW.acquired = 1 AND EXISTS(SELECT 1 FROM runners WHERE job_id=NEW.id AND agent_id > 0)
)
BEGIN SELECT RAISE(ABORT, 'illegal job state transition'); END;
