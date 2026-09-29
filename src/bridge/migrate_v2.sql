CREATE TABLE messages_new (
 seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
 room_id TEXT NOT NULL REFERENCES rooms(id), sender TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('request','reply','notice','system','output','tool','approval')),
 body TEXT NOT NULL, recipients TEXT NOT NULL, reply_to TEXT REFERENCES messages(id),
 depth INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL
);
INSERT INTO messages_new SELECT seq,id,room_id,
 CASE WHEN sender='boss' THEN (SELECT id FROM members WHERE members.room_id=messages.room_id AND role='boss') ELSE sender END,
 kind,body,recipients,reply_to,depth,created FROM messages;
DROP TABLE messages;
ALTER TABLE messages_new RENAME TO messages;
