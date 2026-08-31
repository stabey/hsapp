-- 本地牌组主表(设计文档 §17 decks;活动牌组用 is_active,至多一行=1)
CREATE TABLE decks (
    id                    INTEGER PRIMARY KEY,
    name                  TEXT NOT NULL,
    class                 TEXT NOT NULL,
    format                INTEGER NOT NULL,
    archived              INTEGER NOT NULL DEFAULT 0,
    is_active             INTEGER NOT NULL DEFAULT 0,
    canonical_deckstring  TEXT NOT NULL,
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL
);

CREATE INDEX idx_decks_canonical ON decks (canonical_deckstring);
CREATE UNIQUE INDEX idx_decks_active ON decks (is_active) WHERE is_active = 1;
