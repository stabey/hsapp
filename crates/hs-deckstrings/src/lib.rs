//! `hs-deckstrings` — Deckstring(varint+base64)编解码(设计文档 §10.1)。
//!
//! 格式对齐 `hearthstone.deckstrings`(python-hearthstone):保留字节 `0` + 版本
//! varint `1` + FormatType + 英雄/1 张/2 张/n 张卡 + 可选 sideboard 段。
//! 依赖约定:仅 `hs-types`,不依赖 SQLite/GPUI/日志链路。

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use hs_types::{DbfId, FormatType};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 当前官方 Deckstring 版本(python-hearthstone `DECKSTRING_VERSION`)。
const DECKSTRING_VERSION: i32 = 1;

/// 解码后的牌组列表(dbf_id 表示;显示名/card_id 由卡牌库按 build 解析)。
///
/// 规范化约定(编码与比对前提):`cards` 与 `sideboards` 按 dbf_id 升序,
/// 同一 dbf_id 只出现一次(count 聚合)。[`decode`] 的输出保证已规范化。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeckList {
    pub format: FormatType,
    /// 英雄 dbf_id(通常 1 个)。
    pub heroes: Vec<DbfId>,
    /// (卡牌 dbf_id, 张数)。
    pub cards: Vec<(DbfId, u8)>,
    /// (宿主卡 dbf_id, 侧牌列表)。无侧牌时为空。
    pub sideboards: Vec<(DbfId, Vec<(DbfId, u8)>)>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeckstringError {
    #[error("invalid base64: {0}")]
    Base64(String),
    #[error("invalid varint at byte {0}")]
    Varint(usize),
    #[error("unsupported deckstring version: {0}")]
    UnsupportedVersion(i32),
    #[error("malformed deckstring: {0}")]
    Malformed(String),
}

/// Deckstring 文本 → [`DeckList`](输出已规范化)。
///
/// 容忍首尾空白、中间换行,以及官方粘贴格式里的 `#` 注释行;真正解码的是
/// 第一条 `AAE...` 载荷。版本字节不是 1 时返回 [`DeckstringError::UnsupportedVersion`]。
pub fn decode(deckstring: &str) -> Result<DeckList, DeckstringError> {
    let payload = extract_payload(deckstring);
    let bytes = decode_base64(payload)?;
    decode_bytes(&bytes)
}

/// [`DeckList`] → Deckstring 文本。输入须已规范化(见 [`DeckList`] 文档)。
///
/// 编码前会再规范化一次(排序/聚合),与 python-hearthstone `write_deckstring`
/// 一样总是写出 sideboard 标志位(无侧牌写 `0x00`)。因此对旧文档里「无标志位」
/// 的示例解码后再编码,得到的是带标志位的规范串,内容等价、字面量可能不同。
/// 英雄数量必须为 1,否则 `Malformed`(与参考实现一致)。
pub fn encode(deck: &DeckList) -> Result<String, DeckstringError> {
    let normalized = normalize(deck.clone());
    if normalized.heroes.len() != 1 {
        return Err(DeckstringError::Malformed(format!(
            "unsupported hero count {}",
            normalized.heroes.len()
        )));
    }
    let bytes = encode_bytes(&normalized);
    Ok(B64.encode(bytes))
}

fn extract_payload(input: &str) -> &str {
    let trimmed = input.trim();
    if is_deckstring_token(trimmed) {
        return trimmed;
    }
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if is_deckstring_token(line) {
            return line;
        }
    }
    trimmed
}

fn is_deckstring_token(s: &str) -> bool {
    if s.is_empty() || s.contains(char::is_whitespace) {
        return false;
    }
    s.starts_with("AAE")
        && s.bytes()
            .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' | b'='))
}

fn decode_base64(s: &str) -> Result<Vec<u8>, DeckstringError> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let mut padded = cleaned;
    while !padded.len().is_multiple_of(4) {
        padded.push('=');
    }
    B64.decode(padded.as_bytes())
        .map_err(|e| DeckstringError::Base64(e.to_string()))
}

fn decode_bytes(data: &[u8]) -> Result<DeckList, DeckstringError> {
    if data.is_empty() {
        return Err(DeckstringError::Malformed("empty payload".into()));
    }
    if data[0] != 0 {
        return Err(DeckstringError::Malformed("missing reserved 0 byte".into()));
    }
    let mut pos = 1usize;
    let version = read_varint(data, &mut pos)?;
    let version = i32::try_from(version)
        .map_err(|_| DeckstringError::Malformed("version out of range".into()))?;
    if version != DECKSTRING_VERSION {
        return Err(DeckstringError::UnsupportedVersion(version));
    }

    let format_raw = read_varint(data, &mut pos)?;
    let format_raw = i32::try_from(format_raw)
        .map_err(|_| DeckstringError::Malformed("format out of range".into()))?;
    let format = FormatType::from_raw(format_raw);

    let num_heroes = read_count(data, &mut pos, "heroes")?;
    let mut heroes = Vec::with_capacity(num_heroes);
    for _ in 0..num_heroes {
        heroes.push(read_dbf(data, &mut pos)?);
    }
    heroes.sort_by_key(|h| h.0);

    let mut cards: BTreeMap<i64, u32> = BTreeMap::new();
    let num_x1 = read_count(data, &mut pos, "x1 cards")?;
    for _ in 0..num_x1 {
        let id = read_dbf(data, &mut pos)?;
        *cards.entry(id.0).or_insert(0) += 1;
    }
    let num_x2 = read_count(data, &mut pos, "x2 cards")?;
    for _ in 0..num_x2 {
        let id = read_dbf(data, &mut pos)?;
        *cards.entry(id.0).or_insert(0) += 2;
    }
    let num_xn = read_count(data, &mut pos, "xn cards")?;
    for _ in 0..num_xn {
        let id = read_dbf(data, &mut pos)?;
        let count = read_varint(data, &mut pos)?;
        let add = u32::try_from(count)
            .map_err(|_| DeckstringError::Malformed(format!("card count {count} out of range")))?;
        *cards.entry(id.0).or_insert(0) += add;
    }

    let mut sideboard_flat: Vec<(DbfId, u8, DbfId)> = Vec::new();
    if pos < data.len() && data[pos] == 1 {
        pos += 1;
        let n1 = read_count(data, &mut pos, "x1 sideboards")?;
        for _ in 0..n1 {
            let card = read_dbf(data, &mut pos)?;
            let owner = read_dbf(data, &mut pos)?;
            sideboard_flat.push((card, 1, owner));
        }
        let n2 = read_count(data, &mut pos, "x2 sideboards")?;
        for _ in 0..n2 {
            let card = read_dbf(data, &mut pos)?;
            let owner = read_dbf(data, &mut pos)?;
            sideboard_flat.push((card, 2, owner));
        }
        let nn = read_count(data, &mut pos, "xn sideboards")?;
        for _ in 0..nn {
            let card = read_dbf(data, &mut pos)?;
            let count = read_count_u8(data, &mut pos)?;
            let owner = read_dbf(data, &mut pos)?;
            sideboard_flat.push((card, count, owner));
        }
    }

    Ok(normalize(DeckList {
        format,
        heroes,
        cards: cards
            .into_iter()
            .map(|(id, n)| to_card_row(id, n))
            .collect::<Result<Vec<_>, _>>()?,
        sideboards: group_sideboards(sideboard_flat)?,
    }))
}

fn encode_bytes(deck: &DeckList) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0);
    write_varint(&mut out, DECKSTRING_VERSION as u64);
    write_varint(&mut out, deck.format.to_raw() as u64);

    write_varint(&mut out, deck.heroes.len() as u64);
    for hero in &deck.heroes {
        write_varint(&mut out, hero.0 as u64);
    }

    let (x1, x2, xn) = partition_cards(&deck.cards);
    write_varint(&mut out, x1.len() as u64);
    for id in x1 {
        write_varint(&mut out, id.0 as u64);
    }
    write_varint(&mut out, x2.len() as u64);
    for id in x2 {
        write_varint(&mut out, id.0 as u64);
    }
    write_varint(&mut out, xn.len() as u64);
    for (id, count) in xn {
        write_varint(&mut out, id.0 as u64);
        write_varint(&mut out, u64::from(count));
    }

    if deck.sideboards.is_empty() {
        out.push(0);
        return out;
    }

    out.push(1);
    let flat = flatten_sideboards(&deck.sideboards);
    let (s1, s2, sn) = partition_sideboards(&flat);
    write_varint(&mut out, s1.len() as u64);
    for (card, owner) in s1 {
        write_varint(&mut out, card.0 as u64);
        write_varint(&mut out, owner.0 as u64);
    }
    write_varint(&mut out, s2.len() as u64);
    for (card, owner) in s2 {
        write_varint(&mut out, card.0 as u64);
        write_varint(&mut out, owner.0 as u64);
    }
    write_varint(&mut out, sn.len() as u64);
    for (card, count, owner) in sn {
        write_varint(&mut out, card.0 as u64);
        write_varint(&mut out, u64::from(count));
        write_varint(&mut out, owner.0 as u64);
    }
    out
}

fn normalize(mut list: DeckList) -> DeckList {
    list.heroes.sort_by_key(|h| h.0);
    list.heroes.dedup();

    let mut card_map: BTreeMap<i64, u32> = BTreeMap::new();
    for (id, n) in list.cards {
        if n == 0 {
            continue;
        }
        *card_map.entry(id.0).or_insert(0) += u32::from(n);
    }
    list.cards = card_map
        .into_iter()
        .filter_map(|(id, n)| to_card_row(id, n).ok())
        .collect();

    let mut sb: BTreeMap<i64, BTreeMap<i64, u32>> = BTreeMap::new();
    for (owner, cards) in list.sideboards {
        for (id, n) in cards {
            if n == 0 {
                continue;
            }
            *sb.entry(owner.0).or_default().entry(id.0).or_insert(0) += u32::from(n);
        }
    }
    list.sideboards = sb
        .into_iter()
        .map(|(owner, cards)| {
            let rows: Vec<(DbfId, u8)> = cards
                .into_iter()
                .filter_map(|(id, n)| to_card_row(id, n).ok())
                .collect();
            (DbfId(owner), rows)
        })
        .filter(|(_, cards)| !cards.is_empty())
        .collect();
    list
}

type CardCopies = Vec<(DbfId, u8)>;
type SideboardGroups = Vec<(DbfId, CardCopies)>;
type SideboardFlat = Vec<(DbfId, u8, DbfId)>;

fn group_sideboards(flat: SideboardFlat) -> Result<SideboardGroups, DeckstringError> {
    let mut sb: BTreeMap<i64, BTreeMap<i64, u32>> = BTreeMap::new();
    for (card, count, owner) in flat {
        *sb.entry(owner.0).or_default().entry(card.0).or_insert(0) += u32::from(count);
    }
    sb.into_iter()
        .map(|(owner, cards)| {
            let rows = cards
                .into_iter()
                .map(|(id, n)| to_card_row(id, n))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((DbfId(owner), rows))
        })
        .collect()
}

fn flatten_sideboards(sideboards: &[(DbfId, CardCopies)]) -> SideboardFlat {
    let mut flat = Vec::new();
    for (owner, cards) in sideboards {
        for &(card, count) in cards {
            if count > 0 {
                flat.push((card, count, *owner));
            }
        }
    }
    flat.sort_by_key(|(card, _, owner)| (owner.0, card.0));
    flat
}

fn partition_cards(cards: &[(DbfId, u8)]) -> (Vec<DbfId>, Vec<DbfId>, Vec<(DbfId, u8)>) {
    let mut x1 = Vec::new();
    let mut x2 = Vec::new();
    let mut xn = Vec::new();
    for &(id, n) in cards {
        match n {
            1 => x1.push(id),
            2 => x2.push(id),
            n if n > 0 => xn.push((id, n)),
            _ => {}
        }
    }
    (x1, x2, xn)
}

type OwnerPairs = Vec<(DbfId, DbfId)>;

fn partition_sideboards(flat: &[(DbfId, u8, DbfId)]) -> (OwnerPairs, OwnerPairs, SideboardFlat) {
    let mut x1 = Vec::new();
    let mut x2 = Vec::new();
    let mut xn = Vec::new();
    for &(card, count, owner) in flat {
        match count {
            1 => x1.push((card, owner)),
            2 => x2.push((card, owner)),
            n if n > 0 => xn.push((card, n, owner)),
            _ => {}
        }
    }
    (x1, x2, xn)
}

fn to_card_row(id: i64, n: u32) -> Result<(DbfId, u8), DeckstringError> {
    let count = u8::try_from(n)
        .map_err(|_| DeckstringError::Malformed(format!("card count {n} exceeds 255")))?;
    Ok((DbfId(id), count))
}

fn read_varint(data: &[u8], pos: &mut usize) -> Result<u64, DeckstringError> {
    let start = *pos;
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        if *pos >= data.len() || shift > 63 {
            return Err(DeckstringError::Varint(start));
        }
        let byte = data[*pos];
        *pos += 1;
        result |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
    }
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut to_write = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            to_write |= 0x80;
            out.push(to_write);
        } else {
            out.push(to_write);
            break;
        }
    }
}

fn read_count(data: &[u8], pos: &mut usize, what: &str) -> Result<usize, DeckstringError> {
    let n = read_varint(data, pos)?;
    usize::try_from(n).map_err(|_| DeckstringError::Malformed(format!("{what} count out of range")))
}

fn read_count_u8(data: &[u8], pos: &mut usize) -> Result<u8, DeckstringError> {
    let n = read_varint(data, pos)?;
    u8::try_from(n).map_err(|_| DeckstringError::Malformed(format!("count {n} exceeds 255")))
}

fn read_dbf(data: &[u8], pos: &mut usize) -> Result<DbfId, DeckstringError> {
    let n = read_varint(data, pos)?;
    i64::try_from(n)
        .map(DbfId)
        .map_err(|_| DeckstringError::Malformed("dbf_id out of range".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HearthSim 文档示例(无 sideboard 标志位的旧字面量)。
    /// 来源:https://hearthsim.info/docs/deckstrings/
    /// 对照:`hearthstone.deckstrings.parse_deckstring`(tests/differential/venv)。
    const OFFICIAL_DOCS: &str = "AAEBAQcAAAQBAwIDAwMEAw==";
    /// 同一牌组经当前 python-hearthstone `write_deckstring` 写出(带 sideboard `0x00`)。
    const OFFICIAL_CANONICAL: &str = "AAEBAQcAAAQBAwIDAwMEAwA=";
    /// npm `deckstrings` README / python-hearthstone 侧牌对照。
    const SIDEBOARD: &str = "AAEBAQcBBAMBAgMAAQEF/cQFAAA=";
    /// 标准格式 + 真实 dbf(火球 315 等),由 venv 内 `write_deckstring` 生成。
    const MAGE: &str = "AAECAf0EAasEArsClQMAAA==";
    const TWIST_EMPTY: &str = "AAEEAQcAAAAA";
    const CLASSIC: &str = "AAEDAQcBAQAAAA==";
    const MULTI_SIDEBOARD: &str = "AAEBAQcBCgEUAAEBC2QBDGQBFQPIAQ==";

    fn official_list() -> DeckList {
        DeckList {
            format: FormatType::Wild,
            heroes: vec![DbfId(7)],
            cards: vec![(DbfId(1), 3), (DbfId(2), 3), (DbfId(3), 3), (DbfId(4), 3)],
            sideboards: vec![],
        }
    }

    #[test]
    fn official_docs_vector_decodes() {
        let list = decode(OFFICIAL_DOCS).unwrap();
        assert_eq!(list, official_list());
        assert_eq!(encode(&list).unwrap(), OFFICIAL_CANONICAL);
    }

    #[test]
    fn official_canonical_roundtrip() {
        let list = decode(OFFICIAL_CANONICAL).unwrap();
        assert_eq!(list, official_list());
        assert_eq!(encode(&list).unwrap(), OFFICIAL_CANONICAL);
    }

    #[test]
    fn sideboard_vector_roundtrip() {
        let list = decode(SIDEBOARD).unwrap();
        assert_eq!(list.format, FormatType::Wild);
        assert_eq!(list.heroes, vec![DbfId(7)]);
        assert_eq!(
            list.cards,
            vec![(DbfId(1), 2), (DbfId(2), 2), (DbfId(3), 2), (DbfId(4), 1)]
        );
        assert_eq!(list.sideboards, vec![(DbfId(90749), vec![(DbfId(5), 1)])]);
        assert_eq!(encode(&list).unwrap(), SIDEBOARD);
    }

    #[test]
    fn mage_standard_vector_roundtrip() {
        let list = decode(MAGE).unwrap();
        assert_eq!(list.format, FormatType::Standard);
        assert_eq!(list.heroes, vec![DbfId(637)]);
        assert_eq!(
            list.cards,
            vec![(DbfId(315), 2), (DbfId(405), 2), (DbfId(555), 1)]
        );
        assert!(list.sideboards.is_empty());
        assert_eq!(encode(&list).unwrap(), MAGE);
    }

    #[test]
    fn twist_empty_and_classic_roundtrip() {
        let twist = decode(TWIST_EMPTY).unwrap();
        assert_eq!(twist.format, FormatType::Twist);
        assert!(twist.cards.is_empty());
        assert_eq!(encode(&twist).unwrap(), TWIST_EMPTY);

        let classic = decode(CLASSIC).unwrap();
        assert_eq!(classic.format, FormatType::Classic);
        assert_eq!(classic.cards, vec![(DbfId(1), 1)]);
        assert_eq!(encode(&classic).unwrap(), CLASSIC);
    }

    #[test]
    fn multi_owner_sideboard_groups_and_roundtrips() {
        let list = decode(MULTI_SIDEBOARD).unwrap();
        assert_eq!(
            list.sideboards,
            vec![
                (DbfId(100), vec![(DbfId(11), 1), (DbfId(12), 2)]),
                (DbfId(200), vec![(DbfId(21), 3)]),
            ]
        );
        assert_eq!(encode(&list).unwrap(), MULTI_SIDEBOARD);
    }

    #[test]
    fn paste_comments_and_whitespace_are_stripped() {
        let paste = format!("### 文档示例\n# Class: Warrior\n# Format: Wild\n#\n{OFFICIAL_DOCS}\n");
        assert_eq!(decode(&paste).unwrap(), official_list());
        assert_eq!(
            decode(&format!("  {OFFICIAL_CANONICAL} \n")).unwrap(),
            official_list()
        );
    }

    #[test]
    fn unknown_format_is_preserved() {
        let list = DeckList {
            format: FormatType::Unknown(99),
            heroes: vec![DbfId(7)],
            cards: vec![],
            sideboards: vec![],
        };
        let encoded = encode(&list).unwrap();
        let back = decode(&encoded).unwrap();
        assert_eq!(back.format, FormatType::Unknown(99));
    }

    #[test]
    fn unsupported_version_is_distinct_error() {
        // reserved 0 + varint version 2 + format 1 + 0 heroes + empty cards + no sb
        let bytes = [0u8, 2, 1, 0, 0, 0, 0, 0];
        let s = B64.encode(bytes);
        match decode(&s) {
            Err(DeckstringError::UnsupportedVersion(2)) => {}
            other => panic!("expected UnsupportedVersion(2), got {other:?}"),
        }
    }

    #[test]
    fn reserved_byte_and_base64_and_hero_count_errors() {
        assert!(matches!(
            decode("@@@not-base64@@@"),
            Err(DeckstringError::Base64(_))
        ));
        let bad_reserved = B64.encode([1u8, 1, 1, 0, 0, 0, 0, 0]);
        assert!(matches!(
            decode(&bad_reserved),
            Err(DeckstringError::Malformed(_))
        ));
        let truncated = B64.encode([0u8, 1]);
        assert!(matches!(
            decode(&truncated),
            Err(DeckstringError::Varint(_))
        ));

        let no_hero = DeckList {
            format: FormatType::Wild,
            heroes: vec![],
            cards: vec![],
            sideboards: vec![],
        };
        assert!(matches!(
            encode(&no_hero),
            Err(DeckstringError::Malformed(_))
        ));
    }

    #[test]
    fn encode_normalizes_unsorted_and_duplicate_cards() {
        let messy = DeckList {
            format: FormatType::Wild,
            heroes: vec![DbfId(7)],
            cards: vec![
                (DbfId(4), 1),
                (DbfId(1), 2),
                (DbfId(4), 2),
                (DbfId(2), 3),
                (DbfId(1), 1),
                (DbfId(3), 3),
            ],
            sideboards: vec![],
        };
        assert_eq!(encode(&messy).unwrap(), OFFICIAL_CANONICAL);
    }
}
