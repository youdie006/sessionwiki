-- Synthetic v1 SQLite session, using the existing adapter's schema.
CREATE TABLE session(id TEXT PRIMARY KEY, parent_id TEXT, project_id TEXT,
    directory TEXT, title TEXT, time_created INTEGER, time_updated INTEGER);
CREATE TABLE message(id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
CREATE TABLE part(id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
INSERT INTO session VALUES('ses_v1', NULL, 'p1', '/legacy', 'Legacy title', 1000, 2000);
INSERT INTO message VALUES('m1', 'ses_v1', 1000, '{"role":"user"}');
INSERT INTO part VALUES('p1', 'm1', 'ses_v1', 1000, '{"type":"text","text":"legacy search marker"}');
