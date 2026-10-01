-- Active payloads are bounded by host capacity. Historical idempotency records
-- retain only the key, stage, timestamps and final measurement.
ALTER TABLE node_leases RENAME TO node_leases_legacy;
CREATE TABLE node_leases (
    job_id INTEGER PRIMARY KEY,
    attempt INTEGER NOT NULL,
    phase INTEGER NOT NULL,
    payload TEXT,
    started_at_ms INTEGER,
    heartbeat_at_ms INTEGER,
    measurement TEXT
);
INSERT INTO node_leases(job_id,attempt,phase,payload,measurement)
SELECT job_id,attempt,json_extract(payload,'$.phase'),
       CASE WHEN json_extract(payload,'$.phase')<6 THEN payload END,
       CASE WHEN json_type(payload,'$.measurement')='object'
            THEN json_extract(payload,'$.measurement') END
FROM node_leases_legacy;
DROP TABLE node_leases_legacy;
CREATE INDEX node_leases_active ON node_leases(job_id) WHERE phase<6;
ALTER TABLE placements ADD COLUMN execution_started_at INTEGER;
