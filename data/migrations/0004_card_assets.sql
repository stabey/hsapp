-- 卡图资源缓存元数据（实际图片按需落盘，见设计文档 §9.4）
CREATE TABLE card_assets (
    build_id            INTEGER NOT NULL,
    card_id             TEXT NOT NULL,
    locale              TEXT NOT NULL,
    asset_kind          TEXT NOT NULL,
    resolution          TEXT NOT NULL,
    local_path          TEXT,
    remote_etag         TEXT,
    sha256              TEXT,
    status              TEXT NOT NULL,
    PRIMARY KEY (build_id, card_id, locale, asset_kind, resolution)
);
