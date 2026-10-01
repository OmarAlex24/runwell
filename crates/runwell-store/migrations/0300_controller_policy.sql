CREATE TABLE scheduling_context (
 job_id INTEGER PRIMARY KEY REFERENCES jobs(id), payload TEXT NOT NULL
);
CREATE TABLE scheduler_fairness (singleton INTEGER PRIMARY KEY CHECK(singleton=1), payload TEXT NOT NULL);
INSERT INTO scheduler_fairness VALUES(1, '{"repositories":{},"pull_requests":{}}');
CREATE TABLE dispatch_proposals (
 job_id INTEGER PRIMARY KEY REFERENCES placements(job_id), fairness TEXT NOT NULL,
 accepted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE controller_observations (
 job_id INTEGER PRIMARY KEY REFERENCES jobs(id), reason TEXT, completed INTEGER NOT NULL DEFAULT 0,
 processed INTEGER NOT NULL DEFAULT 0, counted INTEGER NOT NULL DEFAULT 0, infra INTEGER NOT NULL DEFAULT 0, observed_at INTEGER NOT NULL
);
CREATE INDEX pending_observations ON controller_observations(processed,job_id);
CREATE INDEX recent_observations ON controller_observations(observed_at);
CREATE TABLE template_expiry (version TEXT PRIMARY KEY, expires_at INTEGER NOT NULL);
