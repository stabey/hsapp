//! HearthstoneJSON 公开端点（卡牌 JSON + 卡图）。
//!
//! 全部 URL 只允许出现在本模块；调用方通过函数拼路径，禁止手写主机名。
//! 调研结论见任务 `research/a-hsjson.md`。

use hs_types::{BuildId, CardId};

/// 卡牌定义 API 根（含 build / locale / cards.json）。
pub const HSJSON_API_BASE: &str = "https://api.hearthstonejson.com/v1";

/// 卡图 API 根（tile / render）。
pub const HSJSON_ART_BASE: &str = "https://art.hearthstonejson.com/v1";

/// `latest` 目录列表（HTML）。其中第一条 `/v1/<build>` 即当前数字 build。
pub const HSJSON_LATEST_LISTING_URL: &str = "https://api.hearthstonejson.com/v1/latest/";

/// 首版支持的卡图种类（与 `AssetKey.asset_kind` 对齐）。
pub const ASSET_KIND_TILE: &str = "tile";
pub const ASSET_KIND_FULL: &str = "full";

/// 全卡集合（含非 collectible）JSON。
pub fn cards_json_url(build: BuildId, locale: &str) -> String {
    format!("{HSJSON_API_BASE}/{}/{locale}/cards.json", build.0)
}

/// 牌组列表用的预裁 tile（固定 256×59 PNG，URL 不含 resolution）。
pub fn tile_image_url(card_id: &CardId) -> String {
    format!("{HSJSON_ART_BASE}/tiles/{}.png", card_id.as_str())
}

/// 完整卡面渲染。`resolution` 接受 `256`/`512` 或 `256x`/`512x`。
pub fn full_image_url(card_id: &CardId, locale: &str, resolution: &str) -> String {
    format!(
        "{HSJSON_ART_BASE}/render/latest/{locale}/{}/{}.png",
        normalize_render_resolution(resolution),
        card_id.as_str()
    )
}

/// 把 `AssetKey.resolution` 收成 HearthstoneJSON 的 `256x` / `512x`。
///
/// 无法识别时原样返回（失败会在下载层显式暴露，不静默改成默认值）。
pub fn normalize_render_resolution(resolution: &str) -> String {
    match resolution {
        "256" | "256x" => "256x".to_string(),
        "512" | "512x" => "512x".to_string(),
        other => other.to_string(),
    }
}

pub fn is_supported_asset_kind(kind: &str) -> bool {
    kind == ASSET_KIND_TILE || kind == ASSET_KIND_FULL
}

/// 从 `/v1/latest/` 的 HTML 目录页解析数字 build。
///
/// HearthstoneJSON 的 `latest` 不是 JSON manifest，而是一份与当前
/// build 目录相同的 HTML 树；页头第一条 `/v1/<digits>` 即当前 build。
/// 解析失败返回 `None`，并带上未匹配原因，调用方不得假装拿到了 build。
pub fn parse_latest_build_listing(html: &str) -> Result<BuildId, LatestBuildParseError> {
    let mut rest = html;
    while let Some(idx) = rest.find("/v1/") {
        let after = &rest[idx + 4..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            match digits.parse::<u32>() {
                Ok(n) if n > 0 => return Ok(BuildId(n)),
                Ok(_) => {
                    return Err(LatestBuildParseError {
                        reason: "listing 里的 build 为 0".into(),
                    });
                }
                Err(_) => {
                    return Err(LatestBuildParseError {
                        reason: format!("listing 里的 build 无法解析: {digits}"),
                    });
                }
            }
        }
        rest = after;
    }
    Err(LatestBuildParseError {
        reason: "listing HTML 中没有 /v1/<build> 链接".into(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestBuildParseError {
    pub reason: String,
}

impl std::fmt::Display for LatestBuildParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "无法从 HearthstoneJSON latest listing 解析 build: {}",
            self.reason
        )
    }
}

impl std::error::Error for LatestBuildParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_url_uses_build_and_locale() {
        assert_eq!(
            cards_json_url(BuildId(249896), "zhCN"),
            "https://api.hearthstonejson.com/v1/249896/zhCN/cards.json"
        );
    }

    #[test]
    fn tile_url_is_png_by_card_id() {
        assert_eq!(
            tile_image_url(&CardId::new("CS2_029")),
            "https://art.hearthstonejson.com/v1/tiles/CS2_029.png"
        );
    }

    #[test]
    fn full_url_normalizes_resolution() {
        let card = CardId::new("CS2_029");
        assert_eq!(
            full_image_url(&card, "zhCN", "256"),
            "https://art.hearthstonejson.com/v1/render/latest/zhCN/256x/CS2_029.png"
        );
        assert_eq!(
            full_image_url(&card, "zhCN", "512x"),
            "https://art.hearthstonejson.com/v1/render/latest/zhCN/512x/CS2_029.png"
        );
    }

    #[test]
    fn unknown_resolution_is_kept() {
        assert_eq!(normalize_render_resolution("orig"), "orig");
    }

    #[test]
    fn parse_latest_from_realistic_listing() {
        let html = r#"<a href="/v1/249896">/v1/249896</a><br>
	├── <a href="/v1/249896/zhCN/">zhCN</a>"#;
        assert_eq!(parse_latest_build_listing(html).unwrap(), BuildId(249896));
    }

    #[test]
    fn parse_latest_rejects_empty() {
        let err = parse_latest_build_listing("<html>no builds</html>").unwrap_err();
        assert!(err.reason.contains("没有 /v1/<build>"), "{err}");
    }
}
