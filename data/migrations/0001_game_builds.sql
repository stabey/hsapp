-- 游戏 Build 清单：卡牌数据、规则、池定义的版本锚点（设计文档 §9.2/§9.3）
CREATE TABLE game_builds (
    build_id            INTEGER PRIMARY KEY,
    version_text        TEXT,
    imported_at         TEXT NOT NULL,
    source_manifest     TEXT NOT NULL,
    source_hash         TEXT NOT NULL,
    is_current          INTEGER NOT NULL DEFAULT 0
);
