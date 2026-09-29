PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS rooms (
 id TEXT PRIMARY KEY, title TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'active'
 CHECK(state IN ('active','paused','completed','cancelled','archived')),
 target TEXT, integration_path TEXT, integrated_head TEXT, created INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS members (
 id TEXT PRIMARY KEY, room_id TEXT NOT NULL REFERENCES rooms(id), name TEXT NOT NULL,
 role TEXT NOT NULL CHECK(role IN ('boss','coordinator','executor')),
 cli TEXT NOT NULL DEFAULT 'claude' CHECK(cli IN ('claude','codex')),
 provider TEXT NOT NULL DEFAULT '', model TEXT NOT NULL DEFAULT '',
 state TEXT NOT NULL DEFAULT 'configured' CHECK(state IN ('configured','starting','ready','busy','offline','removed')),
 generation INTEGER NOT NULL DEFAULT 0, token_hash TEXT, challenge TEXT,
 inspected INTEGER NOT NULL DEFAULT 0, skill_read INTEGER NOT NULL DEFAULT 0,
 session TEXT, cwd TEXT, error TEXT, UNIQUE(room_id,name)
);
CREATE UNIQUE INDEX IF NOT EXISTS one_coordinator ON members(room_id) WHERE role='coordinator' AND state!='removed';
CREATE UNIQUE INDEX IF NOT EXISTS one_boss ON members(room_id) WHERE role='boss';
CREATE TABLE IF NOT EXISTS messages (
 seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
 room_id TEXT NOT NULL REFERENCES rooms(id), sender TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('request','reply','notice','system','output','tool','approval')),
 body TEXT NOT NULL, recipients TEXT NOT NULL, reply_to TEXT REFERENCES messages(id),
 work_id TEXT, request_id TEXT NOT NULL, fingerprint TEXT NOT NULL,
 depth INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL,
 UNIQUE(sender,request_id)
);
CREATE TABLE IF NOT EXISTS deliveries (
 message_id TEXT NOT NULL REFERENCES messages(id), member_id TEXT NOT NULL REFERENCES members(id),
 state TEXT NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','delivered','processed','cancelled')),
 PRIMARY KEY(message_id,member_id)
);
CREATE TABLE IF NOT EXISTS work (
 id TEXT PRIMARY KEY, room_id TEXT NOT NULL REFERENCES rooms(id), title TEXT NOT NULL,
 owner TEXT REFERENCES members(id), state TEXT NOT NULL DEFAULT 'todo'
 CHECK(state IN ('todo','running','review','done','blocked','cancelled')),
 dependencies TEXT NOT NULL DEFAULT '[]', evidence TEXT NOT NULL DEFAULT '',
 commit_id TEXT, version INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS mutations (
 actor TEXT NOT NULL, request_id TEXT NOT NULL, fingerprint TEXT NOT NULL, result TEXT NOT NULL,
 PRIMARY KEY(actor,request_id)
);
CREATE TABLE IF NOT EXISTS checks (
 id TEXT PRIMARY KEY, room_id TEXT NOT NULL REFERENCES rooms(id), head TEXT NOT NULL,
 argv TEXT NOT NULL, exit_code INTEGER NOT NULL, output TEXT NOT NULL, work_revision INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS approvals (
 id TEXT PRIMARY KEY, member_id TEXT NOT NULL REFERENCES members(id), generation INTEGER NOT NULL,
 details TEXT NOT NULL, decision TEXT CHECK(decision IN ('allow','deny'))
);
CREATE TABLE IF NOT EXISTS processes (
 member_id TEXT PRIMARY KEY REFERENCES members(id), pid INTEGER NOT NULL, fingerprint TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS message_changes (
 seq INTEGER PRIMARY KEY AUTOINCREMENT, message_id TEXT NOT NULL REFERENCES messages(id)
);
CREATE TRIGGER IF NOT EXISTS message_inserted AFTER INSERT ON messages BEGIN
 INSERT INTO message_changes(message_id) VALUES(new.id);
END;
CREATE TRIGGER IF NOT EXISTS message_streamed AFTER UPDATE OF body ON messages BEGIN
 INSERT INTO message_changes(message_id) VALUES(new.id);
END;
CREATE TRIGGER IF NOT EXISTS delivery_changed AFTER UPDATE OF state ON deliveries BEGIN
 INSERT INTO message_changes(message_id) VALUES(new.message_id);
END;
CREATE INDEX IF NOT EXISTS room_messages ON messages(room_id,seq);
CREATE INDEX IF NOT EXISTS member_inbox ON deliveries(member_id,state,message_id);
CREATE INDEX IF NOT EXISTS changed_messages ON message_changes(message_id,seq);
PRAGMA user_version=1;
