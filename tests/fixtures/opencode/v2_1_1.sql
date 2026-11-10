-- Synthetic OpenCode v2.1.1 layout, originally supplied in sessionwiki PR #85
-- by AlexZander85 (e0468cf). No personal data or live database contents.
BEGIN TRANSACTION;
CREATE TABLE session_message (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    type TEXT NOT NULL,
    seq INTEGER NOT NULL,
    time_created INTEGER,
    data TEXT NOT NULL
);
INSERT INTO "session_message" VALUES('msg_001','ses_v2_sanitized_001','user',1,1718630400000,'{"text":"Please update the authentication handler to use JWT verification"}');
INSERT INTO "session_message" VALUES('msg_002','ses_v2_sanitized_001','assistant',2,1718630420000,'{"content":[{"type":"text","text":"I will inspect and update the auth module."},{"type":"tool","name":"write","state":{"status":"completed","input":{"path":"src/auth.ts","content":"export function verifyToken(token: string) { return true; }"},"output":"File written successfully"}},{"type":"text","text":"Authentication has been updated to JWT."}]}');
CREATE TABLE session_v2 (
    id TEXT PRIMARY KEY,
    parent_id TEXT,
    project_id TEXT,
    directory TEXT,
    title TEXT,
    time_created INTEGER,
    time_updated INTEGER
);
INSERT INTO "session_v2" VALUES('ses_v2_sanitized_001',NULL,'prj_demo_001','/home/developer/opencode-v2-demo','Migrate authentication to JWT',1718630400000,1718630500000);
COMMIT;
