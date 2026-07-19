CREATE TABLE chat_messages (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    task_id TEXT NOT NULL,
    sender_type TEXT NOT NULL CHECK (sender_type IN ('agent','operator','system')),
    sender_id TEXT,
    sender_label TEXT NOT NULL CHECK (length(trim(sender_label)) > 0),
    kind TEXT NOT NULL CHECK (kind IN ('progress','intent','question','decision','warning','handoff','note')),
    body TEXT NOT NULL CHECK (length(trim(body)) > 0 AND length(body) <= 8000),
    reply_to_message_id TEXT,
    mentions_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(mentions_json)),
    related_entities_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(related_entities_json)),
    event_sequence INTEGER NOT NULL UNIQUE CHECK (event_sequence > 0),
    created_at INTEGER NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (reply_to_message_id) REFERENCES chat_messages(id) ON DELETE RESTRICT,
    FOREIGN KEY (event_sequence) REFERENCES events(sequence) ON DELETE RESTRICT
);

CREATE INDEX idx_chat_messages_task_sequence
    ON chat_messages(task_id,event_sequence);

CREATE INDEX idx_chat_messages_task_reply
    ON chat_messages(task_id,reply_to_message_id,event_sequence);
