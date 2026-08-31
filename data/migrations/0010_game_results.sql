-- 对局胜负(winner 为 PlayerId;未结束或未知时为空)
CREATE TABLE game_results (
    game_id     INTEGER PRIMARY KEY REFERENCES game_sessions(game_id),
    winner      INTEGER
);
