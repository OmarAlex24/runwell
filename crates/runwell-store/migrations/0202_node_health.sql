CREATE TABLE node_health (
 node_id TEXT PRIMARY KEY,
 last_seen INTEGER NOT NULL,
 lost INTEGER NOT NULL DEFAULT 0
);
INSERT INTO node_health(node_id,last_seen) SELECT node_id,received_at FROM node_snapshots;
