-- 某版本的英雄/主牌/侧牌明细(查询用;权威列表以 deckstring 解码为准)
CREATE TABLE deck_cards (
    version_id        INTEGER NOT NULL REFERENCES deck_versions(id),
    slot              TEXT NOT NULL,
    dbf_id            INTEGER NOT NULL,
    count             INTEGER NOT NULL,
    sideboard_owner   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (version_id, slot, dbf_id, sideboard_owner)
);
