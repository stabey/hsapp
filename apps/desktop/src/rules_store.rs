//! 规则与池定义的全局加载器(M3 Track D)。
//!
//! 目录约定(design §5):`rules/cards/*.yaml`(EffectProgram,多 document)
//! 与 `rules/pools/*.yaml`(PoolSpec,单 document)。
//!
//! 根目录按三级顺序定位(见 [`resolve_rules_dir`]):`HS_RULES_DIR` 环境变量 →
//! 所在 .app 的 `Contents/Resources/rules` → 进程 CWD 下的 `rules/`。
//! 加载失败不阻塞应用,错误进 `errors` 由 Pools 页展示。

use gpui::Global;
use hs_card_db::SqliteCardRepository;
use hs_effect_ir::RuleSet;
use hs_planner::PlaybookCatalog;
use hs_pools::PoolSpec;
use hs_types::BuildId;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;

use crate::pool_query::SqliteCardQuery;

/// Shared immutable pool definitions with slice-like iteration for the
/// existing Inspector and selfcheck call sites.
#[derive(Clone)]
pub struct SharedPoolSpecs(Arc<[PoolSpec]>);

impl SharedPoolSpecs {
    pub fn as_arc(&self) -> Arc<[PoolSpec]> {
        self.0.clone()
    }
}

impl Deref for SharedPoolSpecs {
    type Target = [PoolSpec];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> IntoIterator for &'a SharedPoolSpecs {
    type Item = &'a PoolSpec;
    type IntoIter = std::slice::Iter<'a, PoolSpec>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub struct RulesStore {
    pub rules_dir: PathBuf,
    pub rule_set: Arc<RuleSet>,
    /// 已加载池定义(文件名序;pool_id 展示与求值都用它)。
    pub pools: SharedPoolSpecs,
    /// 生产 CardQuery(独立只读连接;None = 卡牌库未就绪)。
    pub card_query: Option<Arc<SqliteCardQuery>>,
    /// 严格加载并校验后的建议 Playbook；失败时 Advice 不接线。
    #[allow(dead_code)] // `pools_selfcheck` path-includes the store without Advisor.
    pub playbooks: Option<Arc<PlaybookCatalog>>,
    /// 卡牌库当前 build(池求值默认 build)。
    pub current_build: Option<BuildId>,
    /// 加载/打开错误(逐条展示,不阻塞)。
    pub errors: Vec<String>,
    #[allow(dead_code)] // `pools_selfcheck` path-includes the store without Advisor.
    advisor_resources_ready: bool,
}

impl Global for RulesStore {}

/// 覆盖规则目录的环境变量(优先级最高)。
pub const RULES_DIR_ENV: &str = "HS_RULES_DIR";

/// 规则目录的三级定位。第一个命中即用:
///
/// 1. `HS_RULES_DIR` 环境变量(值非空)——手动指定,调试与 CI 用;
/// 2. 所在 .app 的 `Contents/Resources/rules`(且目录确实存在)——打包后启动;
/// 3. 进程 CWD 下的 `rules/`——开发期从仓库根 `cargo run`,与 bundle 化前一致。
///
/// 第 2 级要求目录真实存在:万一包里漏放规则,行为是可解释的降级到第 3 级,
/// 而不是指向一个不存在的路径。
fn resolve_rules_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(RULES_DIR_ENV) {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Some(resources) = hs_platform::bundle_resources_dir() {
        let bundled = resources.join("rules");
        if bundled.is_dir() {
            return bundled;
        }
    }
    PathBuf::from("rules")
}

impl RulesStore {
    pub fn load_default() -> Self {
        let rules_dir = resolve_rules_dir();
        let mut errors = Vec::new();

        let cards_dir = rules_dir.join("cards");
        let (rule_set, rules_ok) = match RuleSet::load_dir(&cards_dir) {
            Ok(set) => (set, true),
            Err(err) => {
                errors.push(format!("规则加载失败 {}: {err}", cards_dir.display()));
                (RuleSet::default(), false)
            }
        };

        let mut pools = Vec::new();
        let mut pools_ok = true;
        let pools_dir = rules_dir.join("pools");
        if pools_dir.is_dir() {
            match std::fs::read_dir(&pools_dir) {
                Ok(entries) => {
                    let mut paths: Vec<PathBuf> = entries
                        .filter_map(|e| e.ok().map(|e| e.path()))
                        .filter(|p| {
                            matches!(
                                p.extension().and_then(|e| e.to_str()),
                                Some("yaml") | Some("yml")
                            )
                        })
                        .collect();
                    paths.sort();
                    for path in paths {
                        match std::fs::read_to_string(&path)
                            .map_err(|e| e.to_string())
                            .and_then(|text| hs_pools::parse_spec(&text).map_err(|e| e.to_string()))
                        {
                            Ok(spec) => pools.push(spec),
                            Err(err) => {
                                pools_ok = false;
                                errors.push(format!("池定义 {} 加载失败: {err}", path.display()))
                            }
                        }
                    }
                }
                Err(err) => {
                    pools_ok = false;
                    errors.push(format!("池目录读取失败: {err}"));
                }
            }
        } else {
            pools_ok = false;
            errors.push(format!("池目录不存在: {}", pools_dir.display()));
        }

        let playbooks = match load_playbooks(&rules_dir.join("playbooks")) {
            Ok(catalog) => Some(Arc::new(catalog)),
            Err(error) => {
                errors.push(error);
                None
            }
        };

        let (card_query, current_build) =
            match SqliteCardRepository::open(hs_platform::default_card_db_path()) {
                Ok(repo) => {
                    let build = repo.current_build().ok().flatten();
                    (Some(Arc::new(SqliteCardQuery::new(repo))), build)
                }
                Err(err) => {
                    errors.push(format!("卡牌库打开失败(池求值不可用): {err}"));
                    (None, None)
                }
            };

        let advisor_resources_ready =
            rules_ok && pools_ok && playbooks.is_some() && card_query.is_some();

        Self {
            rules_dir,
            rule_set: Arc::new(rule_set),
            pools: SharedPoolSpecs(pools.into()),
            card_query,
            playbooks,
            current_build,
            errors,
            advisor_resources_ready,
        }
    }

    #[allow(dead_code)] // `pools_selfcheck` path-includes the store without Advisor.
    pub fn advisor_resources_ready(&self) -> bool {
        self.advisor_resources_ready
    }
}

fn load_playbooks(dir: &std::path::Path) -> Result<PlaybookCatalog, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("Playbook 目录读取失败 {}: {error}", dir.display()))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("Playbook 目录项读取失败 {}: {error}", dir.display()))?;
        let path = entry.path();
        if entry
            .file_type()
            .map_err(|error| format!("Playbook 文件类型读取失败 {}: {error}", path.display()))?
            .is_file()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml")
                })
        {
            paths.push(path);
        }
    }
    paths.sort();

    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        let yaml = std::fs::read_to_string(&path)
            .map_err(|error| format!("Playbook {} 读取失败: {error}", path.display()))?;
        sources.push((path.display().to_string(), yaml));
    }
    PlaybookCatalog::from_sources(sources)
        .map_err(|error| format!("Playbook 加载失败 {}: {error}", dir.display()))
}
