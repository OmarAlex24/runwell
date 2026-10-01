-- Controller placements never move after an ambiguous admission response.
CREATE TABLE placements (
 job_id INTEGER PRIMARY KEY REFERENCES jobs(id),
 attempt INTEGER NOT NULL CHECK(attempt > 0),
 node_id TEXT NOT NULL,
 assigned_at INTEGER NOT NULL,
 lost INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE node_snapshots (
 node_id TEXT PRIMARY KEY,
 sequence INTEGER NOT NULL,
 received_at INTEGER NOT NULL,
 payload TEXT NOT NULL
);
-- A node's local execution spool has no GitHub credentials or JIT material.
CREATE TABLE node_leases (
 job_id INTEGER PRIMARY KEY,
 attempt INTEGER NOT NULL,
 payload TEXT NOT NULL
);
CREATE TABLE node_control (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 draining INTEGER NOT NULL DEFAULT 0,
 sequence INTEGER NOT NULL DEFAULT 0
);
INSERT INTO node_control(singleton) VALUES(1);
CREATE TABLE failure_outbox (
 job_id INTEGER NOT NULL REFERENCES jobs(id),
 attempt INTEGER NOT NULL,
 reason TEXT NOT NULL,
 delivered INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(job_id,attempt)
);
CREATE TABLE jit_intents (job_id INTEGER PRIMARY KEY REFERENCES jobs(id));
