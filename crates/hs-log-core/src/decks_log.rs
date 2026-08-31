//! Decks.log 行/段解析(M2 Track B)。
//!
//! # 格式假设(待真实日志校准)
//!
//! 炉石客户端在选牌组进对局时会往 `Decks.log` 写一段接近官方「粘贴牌组」的文本。
//! 社区(HDT / HearthSim 文档)与客户端导出共用同一骨架,本解析器按该骨架实现:
//!
//! ```text
//! D 19:15:22.1234567 ### 牌组名
//! # Class: Mage
//! # Format: Standard
//! # Hero: HERO_08
//! #
//! # 2x (1) Arcane Missiles
//! #
//! AAECA...deckstring
//! ```
//!
//! 待用真实 `Logs/Decks.log` 核实的点:
//! - 行首是否总是 `D|I HH:MM:SS.fffffff` 前缀,以及是否带 `Logger() - `;
//! - 牌组名是 `### Name` 还是 `Finding Game With Deck: ### Name`;
//! - Class/Format/Hero 注释的键名与本地化(中文「职业/格式」);
//! - 一段是否在 deckstring 之后还有尾部行,多副牌组如何分隔;
//! - 是否出现 JSON / `DeckId=` 结构化行(本实现会记为 Unknown,不丢弃)。
//!
//! 未识别行保留 [`DecksLogRecord::Unknown`],与 Power.log 的 Unknown 风格一致。

/// 一条 Decks.log 记录:识别出的牌组段,或未识别原文。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecksLogRecord {
    Deck(ObservedDeck),
    Unknown { raw: String },
}

/// 这段牌组是怎么出现在 Decks.log 里的。
///
/// 客户端启动会先把收藏整表倒出来(`Collection`),那不代表在排队;
/// 只有 `Finding Game With Deck` 才是本局用的牌。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DeckObserveKind {
    #[default]
    Collection,
    FindingGame,
    Edited,
}

/// 从一段 Decks.log 提取的牌组观察值。所有字段均可缺失。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ObservedDeck {
    pub name: Option<String>,
    pub class: Option<String>,
    pub format: Option<String>,
    pub hero: Option<String>,
    pub deckstring: Option<String>,
    pub kind: DeckObserveKind,
    /// 行首时钟 `HH:MM:SS.frac`,没有日期。
    pub observed_at: Option<String>,
}

/// 流式段解析器:喂入一行,在段结束时吐出记录。
#[derive(Debug, Default)]
pub struct DecksLogParser {
    pending: Option<ObservedDeck>,
    last_kind: DeckObserveKind,
    last_clock: Option<String>,
}

impl DecksLogParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一行。可能因新段开始而冲刷上一副未完成牌组。
    pub fn push(&mut self, line: &str) -> Vec<DecksLogRecord> {
        let mut out = Vec::new();
        self.feed(line, &mut out);
        out
    }

    /// 结束流时冲刷尚未闭合的牌组段。
    pub fn finish(&mut self) -> Vec<DecksLogRecord> {
        let mut out = Vec::new();
        self.flush_pending(&mut out);
        out
    }

    /// 将整段文本解析为记录序列。
    pub fn parse_text(text: &str) -> Vec<DecksLogRecord> {
        let mut parser = Self::new();
        let mut out = Vec::new();
        for line in text.lines() {
            parser.feed(line, &mut out);
        }
        parser.flush_pending(&mut out);
        out
    }

    fn feed(&mut self, line: &str, out: &mut Vec<DecksLogRecord>) {
        let body = strip_log_prefix(line).trim();
        let clock = parse_line_clock(line);

        if let Some(kind) = classify_header(body) {
            self.last_kind = kind;
            if clock.is_some() {
                self.last_clock = clock.clone();
            }
            if parse_title(body).is_none() {
                return;
            }
        }

        if let Some(name) = parse_title(body) {
            self.flush_pending(out);
            self.pending = Some(ObservedDeck {
                name: Some(name),
                kind: self.last_kind,
                observed_at: clock.or_else(|| self.last_clock.clone()),
                ..ObservedDeck::default()
            });
            return;
        }

        if let Some(pending) = &mut self.pending {
            if let Some(class) = parse_hash_field(body, "Class") {
                pending.class = Some(class);
                return;
            }
            if let Some(format) = parse_hash_field(body, "Format") {
                pending.format = Some(format);
                return;
            }
            if let Some(hero) = parse_hash_field(body, "Hero") {
                pending.hero = Some(hero);
                return;
            }
            if is_comment(body) {
                return;
            }
            if is_deckstring_token(body) {
                pending.deckstring = Some(body.to_string());
                if let Some(deck) = self.pending.take() {
                    out.push(DecksLogRecord::Deck(deck));
                }
                return;
            }
        } else if is_deckstring_token(body) {
            out.push(DecksLogRecord::Deck(ObservedDeck {
                deckstring: Some(body.to_string()),
                ..ObservedDeck::default()
            }));
            return;
        }

        if body.is_empty() {
            return;
        }
        self.flush_pending(out);
        out.push(DecksLogRecord::Unknown {
            raw: line.to_string(),
        });
    }

    fn flush_pending(&mut self, out: &mut Vec<DecksLogRecord>) {
        if let Some(deck) = self.pending.take() {
            if deck.name.is_some() || deck.deckstring.is_some() {
                out.push(DecksLogRecord::Deck(deck));
            }
        }
    }
}

/// 去掉 `D 19:15:22.1234567` 以及可选的 `Logger() - ` 前缀。
pub fn strip_log_prefix(line: &str) -> &str {
    let first = match line.chars().next() {
        Some(c) => c,
        None => return line,
    };
    if !matches!(first, 'D' | 'I' | 'W' | 'E') {
        return line;
    }
    let rest = &line[first.len_utf8()..];
    if !rest.starts_with(' ') {
        return line;
    }
    let after_space = &rest[1..];
    let mut ts_end = 0usize;
    for (idx, ch) in after_space.char_indices() {
        if ch.is_ascii_digit() || ch == ':' || ch == '.' {
            ts_end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    if ts_end == 0 || !after_space[..ts_end].contains(':') {
        return line;
    }
    let mut remainder = after_space[ts_end..].trim_start();
    if let Some(idx) = remainder.find(" - ") {
        let left = &remainder[..idx];
        if !left.starts_with('#') && !left.starts_with("AAE") {
            remainder = remainder[idx + 3..].trim_start();
        }
    }
    remainder
}

fn classify_header(body: &str) -> Option<DeckObserveKind> {
    if body.contains("Finding Game With Deck") {
        Some(DeckObserveKind::FindingGame)
    } else if body.contains("Finished Editing") {
        Some(DeckObserveKind::Edited)
    } else if body.contains("Deck Contents Received") {
        Some(DeckObserveKind::Collection)
    } else {
        None
    }
}

/// `D 22:42:16.7813170 ...` → `22:42:16.7813170`
fn parse_line_clock(line: &str) -> Option<String> {
    let first = line.chars().next()?;
    if !matches!(first, 'D' | 'I' | 'W' | 'E') {
        return None;
    }
    let rest = line[first.len_utf8()..].strip_prefix(' ')?;
    let token = rest.split_whitespace().next()?;
    if token.contains(':') {
        Some(token.to_string())
    } else {
        None
    }
}

fn parse_title(body: &str) -> Option<String> {
    let idx = body.find("###")?;
    Some(body[idx + 3..].trim().to_string())
}

fn parse_hash_field(body: &str, key: &str) -> Option<String> {
    let body = body.trim();
    if !body.starts_with('#') || body.starts_with("###") {
        return None;
    }
    let rest = body.trim_start_matches('#').trim();
    let (head, tail) = rest.split_once(':')?;
    if !head.eq_ignore_ascii_case(key) {
        return None;
    }
    Some(tail.trim().to_string())
}

fn is_comment(body: &str) -> bool {
    let body = body.trim();
    body.starts_with('#') && !body.starts_with("###")
}

fn is_deckstring_token(s: &str) -> bool {
    if s.is_empty() || s.contains(char::is_whitespace) {
        return false;
    }
    s.starts_with("AAE")
        && s.bytes()
            .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' | b'='))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自造 fixture,标注:待真实 Logs/Decks.log 校准。
    const FIXTURE: &str = "\
D 19:15:22.1234567 Decks.Something() - ### 标准法
# Class: Mage
# Format: Standard
# Hero: HERO_08
# Year of the Pegasus
#
# 2x (1) Arcane Missiles
#
AAECAf0EAasEArsClQMAAA==
I 19:16:00.0000001 random unmatched line
### 只有名字
# Class: Warrior
D 20:00:00.1 Finding Game With Deck: ### 狂野战
# Format: Wild
AAEBAQcAAAQBAwIDAwMEAwA=
AAEEAQcAAAAA
";

    #[test]
    fn fixture_parses_segments_and_keeps_unknown() {
        let records = DecksLogParser::parse_text(FIXTURE);
        let decks: Vec<_> = records
            .iter()
            .filter_map(|r| match r {
                DecksLogRecord::Deck(d) => Some(d),
                DecksLogRecord::Unknown { .. } => None,
            })
            .collect();
        assert_eq!(decks.len(), 4, "expected 4 deck segments, got {records:?}");

        assert_eq!(decks[0].name.as_deref(), Some("标准法"));
        assert_eq!(decks[0].class.as_deref(), Some("Mage"));
        assert_eq!(decks[0].format.as_deref(), Some("Standard"));
        assert_eq!(decks[0].hero.as_deref(), Some("HERO_08"));
        assert_eq!(
            decks[0].deckstring.as_deref(),
            Some("AAECAf0EAasEArsClQMAAA==")
        );

        assert_eq!(decks[1].name.as_deref(), Some("只有名字"));
        assert_eq!(decks[1].class.as_deref(), Some("Warrior"));
        assert!(decks[1].deckstring.is_none());

        assert_eq!(decks[2].name.as_deref(), Some("狂野战"));
        assert_eq!(decks[2].kind, DeckObserveKind::FindingGame);
        assert_eq!(decks[2].format.as_deref(), Some("Wild"));
        assert_eq!(
            decks[2].deckstring.as_deref(),
            Some("AAEBAQcAAAQBAwIDAwMEAwA=")
        );

        assert!(decks[3].name.is_none());
        assert_eq!(decks[3].deckstring.as_deref(), Some("AAEEAQcAAAAA"));

        let unknowns: Vec<_> = records
            .iter()
            .filter_map(|r| match r {
                DecksLogRecord::Unknown { raw } => Some(raw.as_str()),
                DecksLogRecord::Deck(_) => None,
            })
            .collect();
        assert_eq!(unknowns.len(), 1);
        assert!(unknowns[0].contains("random unmatched line"));
    }

    #[test]
    fn finding_game_header_then_title_is_finding_game() {
        let text = "\
I 22:42:16.7813170 Finding Game With Deck:
I 22:42:16.7813170 ### quest
I 22:42:16.7813170 AAECAda+BQAA
";
        let records = DecksLogParser::parse_text(text);
        match &records[..] {
            [DecksLogRecord::Deck(d)] => {
                assert_eq!(d.name.as_deref(), Some("quest"));
                assert_eq!(d.kind, DeckObserveKind::FindingGame);
                assert_eq!(d.observed_at.as_deref(), Some("22:42:16.7813170"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn missing_fields_are_tolerated() {
        let records = DecksLogParser::parse_text("### 无名组\nAAEBAQcAAAQBAwIDAwMEAwA=\n");
        match &records[..] {
            [DecksLogRecord::Deck(d)] => {
                assert_eq!(d.name.as_deref(), Some("无名组"));
                assert!(d.class.is_none());
                assert!(d.format.is_none());
                assert!(d.hero.is_none());
                assert!(d.deckstring.is_some());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn streaming_finish_emits_title_only_deck() {
        let mut parser = DecksLogParser::new();
        assert!(parser.push("### 半截").is_empty());
        let done = parser.finish();
        assert_eq!(
            done,
            vec![DecksLogRecord::Deck(ObservedDeck {
                name: Some("半截".into()),
                ..ObservedDeck::default()
            })]
        );
    }
}
