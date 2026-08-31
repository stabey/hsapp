//! 仓库 `rules/cards/` 的 CI 校验:格式错误在 cargo test 阶段暴露,
//! 不等到应用运行时。目录缺失时静默跳过(下游仅拷贝 crate 的场景)。

use hs_effect_ir::RuleSet;
use std::path::PathBuf;

fn repo_rules_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/cards")
}

#[test]
fn repo_card_rules_all_parse_and_load() {
    let dir = repo_rules_dir();
    if !dir.is_dir() {
        return;
    }
    let set = RuleSet::load_dir(&dir).expect("rules/cards 下的规则文件必须全部可解析");
    assert!(!set.is_empty(), "rules/cards 存在但没加载到任何规则");
}
