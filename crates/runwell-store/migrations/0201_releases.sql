CREATE TABLE controller_release (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 version TEXT NOT NULL,
 checked_at INTEGER NOT NULL
);
