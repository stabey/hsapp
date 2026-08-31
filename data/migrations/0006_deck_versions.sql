-- 牌组版本(每次 create/update 追加一版,保留历史列表)
CREATE TABLE deck_versions (
    id              INTEGER PRIMARY KEY,
    deck_id         INTEGER NOT NULL REFERENCES decks(id),
    version         INTEGER NOT NULL,
    deckstring      TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    UNIQUE (deck_id, version)
);
