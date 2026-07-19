CREATE TABLE chat_message_receipts (
    session_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    acknowledged_at INTEGER NOT NULL,
    PRIMARY KEY (session_id,message_id),
    FOREIGN KEY (session_id) REFERENCES agent_sessions(id) ON DELETE CASCADE,
    FOREIGN KEY (message_id) REFERENCES chat_messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_chat_receipts_message
    ON chat_message_receipts(message_id,session_id);
