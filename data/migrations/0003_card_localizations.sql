-- 卡牌本地化文本
CREATE TABLE card_localizations (
    build_id            INTEGER NOT NULL,
    card_id             TEXT NOT NULL,
    locale              TEXT NOT NULL,
    name                TEXT,
    text                TEXT,
    flavor              TEXT,
    PRIMARY KEY (build_id, card_id, locale)
);
