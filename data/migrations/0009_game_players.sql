-- 对局双方(role=friendly|opposing;player_id 未识别时为空)
CREATE TABLE game_players (
    game_id     INTEGER NOT NULL REFERENCES game_sessions(game_id),
    role        TEXT NOT NULL,
    player_id   INTEGER,
    class       TEXT,
    PRIMARY KEY (game_id, role)
);
