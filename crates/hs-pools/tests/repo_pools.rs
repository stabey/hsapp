//! 仓库 `rules/pools/` 的 CI 校验(与 hs-effect-ir 的 repo_rules 同旨)。

use std::path::PathBuf;

fn repo_pools_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/pools")
}

#[test]
fn repo_pool_specs_all_parse() {
    let dir = repo_pools_dir();
    if !dir.is_dir() {
        return;
    }
    let mut count = 0usize;
    for entry in std::fs::read_dir(&dir).expect("读取 rules/pools") {
        let path = entry.expect("目录项").path();
        let ext = path.extension().and_then(|e| e.to_str());
        if !matches!(ext, Some("yaml") | Some("yml")) {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("读取池定义");
        let spec = hs_pools::parse_spec(&text)
            .unwrap_or_else(|e| panic!("{} 解析失败: {e}", path.display()));
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        assert_eq!(
            spec.pool_id.as_str(),
            stem,
            "{} 的 pool_id 应与文件名一致",
            path.display()
        );
        count += 1;
    }
    assert!(count > 0, "rules/pools 存在但没有池定义文件");
}
