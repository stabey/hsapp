//! `hs-card-db` — 按 Build 版本化的卡牌数据库与卡图缓存元数据（设计文档 §9）。
//!
//! 约束：本 crate 不操作 Overlay、不依赖 GPUI；下载失败不得阻塞日志追踪。

pub mod endpoints;
pub mod image_cache;
pub mod image_fetcher;
pub mod migrate;
pub mod repository;

pub use image_cache::{image_cache_key, image_cache_path, DiskImageCache, IMAGE_CACHE_ROOT};
pub use image_fetcher::HttpImageFetcher;
pub use repository::{
    CardAssetRow, CardLocalization, CardRecord, GameBuildRecord, ImportStats, PoolCardFilter,
    PoolCardRow, SqliteCardRepository,
};

use hs_types::{BuildId, CardId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 卡牌定义（G0 最小投影；raw_json 永远保留完整原文）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CardDefinition {
    pub build_id: BuildId,
    pub card_id: CardId,
    pub dbf_id: Option<i64>,
    pub card_type: Option<String>,
    pub cost: Option<i32>,
    pub attack: Option<i32>,
    pub health: Option<i32>,
    pub collectible: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CardDbError {
    #[error("card not found: {build:?}/{card}")]
    NotFound { build: BuildId, card: CardId },
    #[error("migration failed: {0}")]
    Migration(String),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// 卡牌查询契约（G0 冻结签名；Track B 实装 SQLite 版本）。
pub trait CardRepository {
    fn list_builds(&self) -> Result<Vec<BuildId>, CardDbError>;
    fn card(&self, build: BuildId, card_id: &CardId) -> Result<CardDefinition, CardDbError>;
    fn card_name(
        &self,
        build: BuildId,
        card_id: &CardId,
        locale: &str,
    ) -> Result<Option<String>, CardDbError>;
}

/// 卡图缓存查询结果。
#[derive(Clone, Debug)]
pub enum ImageCacheStatus {
    /// 磁盘命中
    Hit(PathBuf),
    /// 未缓存（调用方显示占位图；下载由 M1+ 的后台任务负责）
    Miss,
    /// 近期下载失败，处于退避窗口
    Backoff,
}

/// 卡图缓存契约（设计文档 §9.4；G0 仅契约 + 占位实现）。
pub trait ImageCache {
    fn lookup(
        &self,
        build: BuildId,
        card_id: &CardId,
        locale: &str,
        asset_kind: &str,
        resolution: &str,
    ) -> Result<ImageCacheStatus, CardDbError>;
}

/// 卡图资产键（缓存与下载的统一寻址,§9.4）。
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct AssetKey {
    pub build: BuildId,
    pub card_id: CardId,
    pub locale: String,
    /// 首版仅 "tile" / "full"（M2 PRD 需求）。
    pub asset_kind: String,
    pub resolution: String,
}

/// 卡图按需下载契约（G-M2 冻结;M2 Track A 实装:
/// 下载→校验→原子替换→更新 card_assets 状态,失败退避）。
///
/// 约束:实现必须在后台线程执行网络 IO,失败不得阻塞启动与日志追踪。
pub trait ImageFetcher {
    /// 确保资产可用:命中直接返回;未命中触发(或合并)后台下载并先返回
    /// `Miss`/`Backoff`,完成后由下次调用返回 `Hit`。
    fn ensure(&self, key: &AssetKey) -> Result<ImageCacheStatus, CardDbError>;
}

/// G0 占位实现：一律 Miss（UI 显示占位图，不阻塞任何流程）。
#[derive(Debug, Default)]
pub struct PlaceholderImageCache;

impl ImageCache for PlaceholderImageCache {
    fn lookup(
        &self,
        _build: BuildId,
        _card_id: &CardId,
        _locale: &str,
        _asset_kind: &str,
        _resolution: &str,
    ) -> Result<ImageCacheStatus, CardDbError> {
        Ok(ImageCacheStatus::Miss)
    }
}
