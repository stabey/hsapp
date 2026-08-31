//! 卡图磁盘缓存键（TB-03）。布局：
//! `data/cache/images/<build>/<card>/<locale>/<kind>-<resolution>`
//!
//! 本轮不下载；[`crate::PlaceholderImageCache`] 仍一律 Miss。

use crate::{CardDbError, ImageCache, ImageCacheStatus};
use hs_types::{BuildId, CardId};
use std::path::{Path, PathBuf};

/// 工作区约定的卡图缓存根目录（相对进程 cwd）。
pub const IMAGE_CACHE_ROOT: &str = "data/cache/images";

/// 相对缓存根的磁盘键：`<build>/<card>/<locale>/<kind>-<resolution>`。
pub fn image_cache_key(
    build: BuildId,
    card_id: &CardId,
    locale: &str,
    asset_kind: &str,
    resolution: &str,
) -> PathBuf {
    PathBuf::from(build.0.to_string())
        .join(card_id.as_str())
        .join(locale)
        .join(format!("{asset_kind}-{resolution}"))
}

/// `root` 下的绝对/完整缓存路径。
pub fn image_cache_path(
    root: impl AsRef<Path>,
    build: BuildId,
    card_id: &CardId,
    locale: &str,
    asset_kind: &str,
    resolution: &str,
) -> PathBuf {
    root.as_ref().join(image_cache_key(
        build, card_id, locale, asset_kind, resolution,
    ))
}

/// 只查磁盘、不下载的缓存实现。文件存在 → Hit，否则 Miss。
#[derive(Clone, Debug)]
pub struct DiskImageCache {
    root: PathBuf,
}

impl DiskImageCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn with_default_root() -> Self {
        Self::new(IMAGE_CACHE_ROOT)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl ImageCache for DiskImageCache {
    fn lookup(
        &self,
        build: BuildId,
        card_id: &CardId,
        locale: &str,
        asset_kind: &str,
        resolution: &str,
    ) -> Result<ImageCacheStatus, CardDbError> {
        let path = image_cache_path(&self.root, build, card_id, locale, asset_kind, resolution);
        if path.is_file() {
            Ok(ImageCacheStatus::Hit(path))
        } else {
            Ok(ImageCacheStatus::Miss)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PlaceholderImageCache;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn key_matches_prd_layout() {
        let card = CardId::new("CS2_029");
        let key = image_cache_key(BuildId(999999), &card, "zhCN", "render", "512");
        assert_eq!(
            key,
            PathBuf::from("999999")
                .join("CS2_029")
                .join("zhCN")
                .join("render-512")
        );

        let path = image_cache_path(
            IMAGE_CACHE_ROOT,
            BuildId(999999),
            &card,
            "zhCN",
            "tile",
            "256",
        );
        assert_eq!(
            path,
            PathBuf::from("data/cache/images/999999/CS2_029/zhCN/tile-256")
        );
    }

    #[test]
    fn placeholder_always_misses_even_if_file_exists() {
        let card = CardId::new("CS2_029");
        let cache = PlaceholderImageCache;
        let status = cache
            .lookup(BuildId(1), &card, "zhCN", "art", "orig")
            .unwrap();
        assert!(matches!(status, ImageCacheStatus::Miss));
    }

    #[test]
    fn disk_cache_hit_and_miss() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hs-card-db-img-{stamp}"));
        fs::create_dir_all(&dir).unwrap();

        let cache = DiskImageCache::new(&dir);
        let card = CardId::new("CS2_029");
        let miss = cache
            .lookup(BuildId(9), &card, "zhCN", "tile", "256")
            .unwrap();
        assert!(matches!(miss, ImageCacheStatus::Miss));

        let path = image_cache_path(&dir, BuildId(9), &card, "zhCN", "tile", "256");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"png").unwrap();

        let hit = cache
            .lookup(BuildId(9), &card, "zhCN", "tile", "256")
            .unwrap();
        match hit {
            ImageCacheStatus::Hit(p) => assert_eq!(p, path),
            other => panic!("expected Hit, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
