-- 对局会话(开始/结束/原始日志段路径+sha256)
CREATE TABLE game_sessions (
    game_id          INTEGER PRIMARY KEY,
    deck_id          INTEGER REFERENCES decks(id),
    started_at       TEXT NOT NULL,
    ended_at         TEXT,
    raw_log_path     TEXT,
    raw_log_sha256   TEXT
);

CREATE INDEX idx_game_sessions_deck ON game_sessions (deck_id);
