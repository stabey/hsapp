-- 卡牌定义：主键 (build_id, card_id)，历史录像永远查得到当年的数据
CREATE TABLE card_definitions (
    build_id            INTEGER NOT NULL,
    card_id             TEXT NOT NULL,
    dbf_id              INTEGER,
    card_type           TEXT,
    card_set            TEXT,
    rarity              TEXT,
    card_class_json     TEXT,
    cost                INTEGER,
    attack              INTEGER,
    health              INTEGER,
    collectible         INTEGER NOT NULL,
    raw_json            TEXT NOT NULL,
    PRIMARY KEY (build_id, card_id)
);

CREATE INDEX idx_card_definitions_dbf ON card_definitions (build_id, dbf_id);
