//! RuleSet:规则文件目录加载 + (BuildId, CardId) → EffectProgram 版本选择。
//!
//! 文件组织(design §5):`rules/cards/<card_id>.yaml`,同一 card_id 的多个
//! build 版本用同文件多 YAML document(`---` 分隔)。

use crate::{validate_program, EffectProgram, IrError};
use hs_types::{BuildId, BuildRange, CardId};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// 已加载的规则集合。加载期校验重复定义(同 card_id 且 build_range 重叠报错)。
#[derive(Debug, Default)]
pub struct RuleSet {
    programs: HashMap<CardId, Vec<EffectProgram>>,
}

impl RuleSet {
    /// 从目录加载全部 `*.yaml` / `*.yml` 规则文件(不递归;非 yaml 扩展名忽略)。
    ///
    /// 每个文件可含多个 YAML document(`---` 分隔);逐文档走与
    /// [`crate::parse_program`] 同等的校验后再 [`Self::insert`]。
    pub fn load_dir(dir: &Path) -> Result<Self, IrError> {
        let mut set = Self::default();
        let mut entries: Vec<std::fs::DirEntry> =
            std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            if !is_yaml_file(&path) {
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            load_yaml_documents(&text, &mut set)?;
        }
        Ok(set)
    }

    /// 追加单个已解析 program(load_dir 内部与测试共用入口)。
    pub fn insert(&mut self, program: EffectProgram) -> Result<(), IrError> {
        let card_id = program.card_id.clone();
        if let Some(existing) = self.programs.get(&card_id) {
            if existing
                .iter()
                .any(|other| ranges_overlap(&other.build_range, &program.build_range))
            {
                return Err(IrError::DuplicateRule { card_id });
            }
        }
        self.programs.entry(card_id).or_default().push(program);
        Ok(())
    }

    /// 选择 build 生效版本:build_range 包含 build 的定义中取 min 最大者。
    pub fn select(&self, build: BuildId, card_id: &CardId) -> Option<&EffectProgram> {
        self.programs.get(card_id).and_then(|programs| {
            programs
                .iter()
                .filter(|program| program.build_range.contains(build))
                .max_by_key(|program| program.build_range.min)
        })
    }

    /// 遍历全部已加载规则(卡序不保证;Inspector 列表/覆盖率报表用,
    /// 调用方需要稳定序时自行排序)。
    pub fn iter(&self) -> impl Iterator<Item = (&CardId, &[EffectProgram])> {
        self.programs
            .iter()
            .map(|(card, programs)| (card, programs.as_slice()))
    }

    /// 已加载的 card 数(状态汇总/覆盖率报表用)。
    pub fn len(&self) -> usize {
        self.programs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.programs.is_empty()
    }
}

/// 闭区间重叠:一方的 `min` 落在另一方内(含 `max: None` = 正向无穷)。
fn ranges_overlap(left: &BuildRange, right: &BuildRange) -> bool {
    left.contains(right.min) || right.contains(left.min)
}

fn is_yaml_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"))
}

/// 按 document 反序列化并校验,再写入 `set`。
fn load_yaml_documents(text: &str, set: &mut RuleSet) -> Result<(), IrError> {
    for document in serde_yaml::Deserializer::from_str(text) {
        let program = EffectProgram::deserialize(document)?;
        validate_program(&program)?;
        set.insert(program)?;
    }
    Ok(())
}
