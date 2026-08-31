//! Power.log 流式 Parser（M0 TC-02 + M1 S2）。
//!
//! 只输出 Packet，不修改业务状态。多行 Packet（CREATE_GAME / FULL_ENTITY /
//! SHOW_ENTITY / CHANGE_ENTITY / Options / Choices / Send*）把后续子行
//! 组装进同一包；无法识别的行保留为 `Packet::Unknown`，禁止丢弃。
//!
//! 来源策略：只消费 `GameState.*` 日志器。`PowerTaskList.DebugPrintPower`
//! 与 GameState 镜像同一事件，消费会导致重复；按 M0 现状将其视为
//! 未识别行（Unknown）。

use crate::game_tag;
use crate::packet::{
    BlockEndPacket, BlockStartPacket, ChangeEntityPacket, ChoicesPacket, ChosenEntitiesPacket,
    CreateGameEntity, CreateGamePacket, CreateGamePlayer, FullEntityPacket, GameMetadataPacket,
    HideEntityPacket, OptionAvailability, OptionEntry, OptionTarget, OptionsPacket, Packet, RawTag,
    SendChoicesPacket, SendOptionPacket, ShowEntityPacket, TagChangePacket, UnknownPacket,
};
use hs_log_record::{ParseStatus, RawLogRecord};
use hs_types::{CardId, EntityId, PlayerId};

const POWER_MARKER: &str = "GameState.DebugPrintPower() - ";
const OPTIONS_MARKER: &str = "GameState.DebugPrintOptions() - ";
const SEND_OPTION_MARKER: &str = "GameState.SendOption() - ";
const ENTITY_CHOICES_MARKER: &str = "GameState.DebugPrintEntityChoices() - ";
const ENTITIES_CHOSEN_MARKER: &str = "GameState.DebugPrintEntitiesChosen() - ";
const SEND_CHOICES_MARKER: &str = "GameState.SendChoices() - ";

#[derive(Debug, Default)]
pub struct PowerLogParser {
    pending: Option<Pending>,
    /// 当前 Options 节点路径：`(缩进, 同层下标)`，用于无损组装递归 subOption。
    option_path: Vec<(usize, usize)>,
}

#[derive(Debug)]
enum Pending {
    CreateGame(CreateGamePacket),
    FullEntity(FullEntityPacket),
    ShowEntity(ShowEntityPacket),
    ChangeEntity(ChangeEntityPacket),
    Options(OptionsPacket),
    Choices(ChoicesPacket),
    ChosenEntities(ChosenEntitiesPacket),
    SendOption(SendOptionPacket),
    SendChoices(SendChoicesPacket),
}

impl PowerLogParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一行。完成的 Packet（含因新顶层语句而冲刷出的上一包）被返回。
    pub fn push(&mut self, record: RawLogRecord) -> Vec<Packet> {
        let mut out = Vec::new();
        self.feed(record, &mut out);
        out
    }

    /// 结束流时冲刷尚未闭合的多行 Packet。
    pub fn finish(&mut self) -> Vec<Packet> {
        self.flush()
    }

    /// 当前是否有可由 quiet window 提交的 Options/Choices 决策包。
    pub fn has_pending_decision(&self) -> bool {
        matches!(
            self.pending,
            Some(Pending::Options(_) | Pending::Choices(_))
        )
    }

    /// 只提交 pending Options/Choices，不触碰其它尚未闭合的多行 Packet。
    pub fn flush_pending_decision(&mut self) -> Vec<Packet> {
        if self.has_pending_decision() {
            self.flush()
        } else {
            Vec::new()
        }
    }

    /// 将一组记录解析为确定的 Packet 序列。
    pub fn parse_records<I>(records: I) -> Vec<Packet>
    where
        I: IntoIterator<Item = RawLogRecord>,
    {
        let mut parser = Self::new();
        let mut out = Vec::new();
        for record in records {
            parser.feed(record, &mut out);
        }
        out.extend(parser.flush());
        out
    }

    fn feed(&mut self, record: RawLogRecord, out: &mut Vec<Packet>) {
        let line = record.raw_line.to_string();
        match line_source(&line) {
            LineSource::Power(payload) => self.feed_power(payload, record, out),
            LineSource::Options(payload) => self.feed_options(payload, record, out),
            LineSource::SendOption(payload) => self.feed_send_option(payload, record, out),
            LineSource::EntityChoices(payload) => self.feed_choices(payload, record, out),
            LineSource::EntitiesChosen(payload) => self.feed_chosen(payload, record, out),
            LineSource::SendChoices(payload) => self.feed_send_choices(payload, record, out),
            LineSource::GameMeta(payload) => {
                out.extend(self.flush());
                out.push(Packet::GameMetadata(parse_game_metadata(record, payload)));
            }
            LineSource::Other => {
                out.extend(self.flush());
                out.push(unknown(record));
            }
        }
    }

    fn feed_power(&mut self, payload: &str, record: RawLogRecord, out: &mut Vec<Packet>) {
        if !self.pending_is_power() {
            out.extend(self.flush());
        }

        let trimmed = payload.trim_start().to_string();
        if is_toplevel_keyword(&trimmed) {
            out.extend(self.flush());
            match classify(&trimmed) {
                LineKind::CreateGame => {
                    let mut packet = CreateGamePacket::default();
                    packet.records.push(mark(record, ParseStatus::Parsed));
                    self.pending = Some(Pending::CreateGame(packet));
                }
                LineKind::FullEntity => {
                    let mut packet = parse_full_entity_head(&trimmed);
                    packet.records.push(mark(record, ParseStatus::Parsed));
                    self.pending = Some(Pending::FullEntity(packet));
                }
                LineKind::ShowEntity => {
                    let mut packet = parse_show_entity_head(&trimmed);
                    packet.records.push(mark(record, ParseStatus::Parsed));
                    self.pending = Some(Pending::ShowEntity(packet));
                }
                LineKind::ChangeEntity => {
                    let mut packet = parse_change_entity_head(&trimmed);
                    packet.records.push(mark(record, ParseStatus::Parsed));
                    self.pending = Some(Pending::ChangeEntity(packet));
                }
                LineKind::HideEntity => {
                    out.push(Packet::HideEntity(parse_hide_entity(record, &trimmed)));
                }
                LineKind::BlockStart => {
                    out.push(Packet::BlockStart(parse_block_start(record, &trimmed)));
                }
                LineKind::BlockEnd => {
                    out.push(Packet::BlockEnd(BlockEndPacket {
                        records: vec![mark(record, ParseStatus::Parsed)],
                    }));
                }
                LineKind::TagChange => {
                    out.push(Packet::TagChange(parse_tag_change(record, &trimmed)));
                }
                _ => out.push(unknown(record)),
            }
            return;
        }

        if self.can_attach_power(&trimmed) {
            self.attach_power(&trimmed, record);
            return;
        }
        out.extend(self.flush());
        out.push(unknown(record));
    }

    fn feed_options(&mut self, payload: &str, record: RawLogRecord, out: &mut Vec<Packet>) {
        let trimmed = payload.trim_start();
        if trimmed.starts_with("id=") {
            out.extend(self.flush());
            let mut packet = parse_options_head(trimmed);
            packet.records.push(mark(record, ParseStatus::Parsed));
            self.pending = Some(Pending::Options(packet));
            self.option_path.clear();
            return;
        }
        if is_options_child(trimmed) {
            if !matches!(self.pending, Some(Pending::Options(_))) {
                out.extend(self.flush());
                out.push(unknown(record));
                return;
            }
            if !self.try_attach_options(payload, record.clone()) {
                out.push(unknown(record));
            }
            return;
        }
        out.extend(self.flush());
        out.push(unknown(record));
    }

    fn feed_send_option(&mut self, payload: &str, record: RawLogRecord, out: &mut Vec<Packet>) {
        let trimmed = payload.trim_start();
        let pairs = parse_pairs(trimmed);
        if pairs.is_empty() && !trimmed.is_empty() {
            out.extend(self.flush());
            out.push(unknown(record));
            return;
        }
        if !send_option_numeric_fields_are_valid(&pairs) {
            match self.pending.take() {
                Some(Pending::SendOption(packet)) => {
                    out.extend(packet.records.into_iter().map(unknown));
                }
                Some(pending) => {
                    self.pending = Some(pending);
                    out.extend(self.flush());
                }
                None => {}
            }
            self.option_path.clear();
            out.push(unknown(record));
            return;
        }
        let has_selection = find_pair(&pairs, "selectedOption").is_some();
        let is_id_head = trimmed.starts_with("id=");
        let already_selected = matches!(&self.pending, Some(Pending::SendOption(packet)) if send_option_has_selection(packet));
        if !matches!(self.pending, Some(Pending::SendOption(_)))
            || is_id_head
            || (has_selection && already_selected)
        {
            out.extend(self.flush());
            self.pending = Some(Pending::SendOption(SendOptionPacket::default()));
        }
        self.attach_send_option(&pairs, record);
        if matches!(
            &self.pending,
            Some(Pending::SendOption(packet)) if send_option_has_complete_selection(packet)
        ) {
            out.extend(self.flush());
        }
    }

    fn feed_choices(&mut self, payload: &str, record: RawLogRecord, out: &mut Vec<Packet>) {
        let trimmed = payload.trim_start();
        if trimmed.starts_with("id=") {
            out.extend(self.flush());
            let mut packet = parse_choices_head(trimmed);
            packet.records.push(mark(record, ParseStatus::Parsed));
            self.pending = Some(Pending::Choices(packet));
            return;
        }
        if is_choices_child(trimmed) {
            if !matches!(self.pending, Some(Pending::Choices(_))) {
                out.extend(self.flush());
                out.push(unknown(record));
                return;
            }
            self.attach_choices(trimmed, record);
            return;
        }
        out.extend(self.flush());
        out.push(unknown(record));
    }

    fn feed_chosen(&mut self, payload: &str, record: RawLogRecord, out: &mut Vec<Packet>) {
        let trimmed = payload.trim_start();
        if trimmed.starts_with("id=") {
            out.extend(self.flush());
            let mut packet = parse_chosen_head(trimmed);
            packet.records.push(mark(record, ParseStatus::Parsed));
            self.pending = Some(Pending::ChosenEntities(packet));
            return;
        }
        if is_chosen_child(trimmed) {
            if !matches!(self.pending, Some(Pending::ChosenEntities(_))) {
                out.extend(self.flush());
                out.push(unknown(record));
                return;
            }
            self.attach_chosen(trimmed, record);
            return;
        }
        out.extend(self.flush());
        out.push(unknown(record));
    }

    fn feed_send_choices(&mut self, payload: &str, record: RawLogRecord, out: &mut Vec<Packet>) {
        let trimmed = payload.trim_start();
        if trimmed.starts_with("id=") {
            out.extend(self.flush());
            let mut packet = parse_send_choices_head(trimmed);
            packet.records.push(mark(record, ParseStatus::Parsed));
            self.pending = Some(Pending::SendChoices(packet));
            return;
        }
        if is_send_choices_child(trimmed) {
            if !matches!(self.pending, Some(Pending::SendChoices(_))) {
                out.extend(self.flush());
                out.push(unknown(record));
                return;
            }
            self.attach_send_choices(trimmed, record);
            return;
        }
        out.extend(self.flush());
        out.push(unknown(record));
    }

    fn pending_is_power(&self) -> bool {
        matches!(
            self.pending,
            None | Some(
                Pending::CreateGame(_)
                    | Pending::FullEntity(_)
                    | Pending::ShowEntity(_)
                    | Pending::ChangeEntity(_)
            )
        )
    }

    fn can_attach_power(&self, trimmed: &str) -> bool {
        match classify(trimmed) {
            LineKind::GameEntity | LineKind::Player => {
                matches!(self.pending, Some(Pending::CreateGame(_)))
            }
            LineKind::Tag => {
                parse_tag_line(trimmed).is_some()
                    && matches!(
                        self.pending,
                        Some(
                            Pending::CreateGame(_)
                                | Pending::FullEntity(_)
                                | Pending::ShowEntity(_)
                                | Pending::ChangeEntity(_)
                        )
                    )
            }
            _ => false,
        }
    }

    fn attach_power(&mut self, trimmed: &str, record: RawLogRecord) {
        match classify(trimmed) {
            LineKind::GameEntity => {
                let Some(Pending::CreateGame(packet)) = self.pending.as_mut() else {
                    return;
                };
                packet.game_entity = Some(parse_game_entity(trimmed));
                packet.records.push(mark(record, ParseStatus::Parsed));
            }
            LineKind::Player => {
                let Some(Pending::CreateGame(packet)) = self.pending.as_mut() else {
                    return;
                };
                packet.players.push(parse_player(trimmed));
                packet.records.push(mark(record, ParseStatus::Parsed));
            }
            LineKind::Tag => {
                let Some(tag) = parse_tag_line(trimmed) else {
                    return;
                };
                match self.pending.as_mut() {
                    Some(Pending::CreateGame(packet)) => {
                        attach_create_game_tag(packet, tag);
                        packet.records.push(mark(record, ParseStatus::Parsed));
                    }
                    Some(Pending::FullEntity(packet)) => {
                        packet.tags.push(tag);
                        packet.records.push(mark(record, ParseStatus::Parsed));
                    }
                    Some(Pending::ShowEntity(packet)) => {
                        packet.tags.push(tag);
                        packet.records.push(mark(record, ParseStatus::Parsed));
                    }
                    Some(Pending::ChangeEntity(packet)) => {
                        packet.tags.push(tag);
                        packet.records.push(mark(record, ParseStatus::Parsed));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn try_attach_options(&mut self, payload: &str, record: RawLogRecord) -> bool {
        let indent = leading_indent(payload);
        let trimmed = payload.trim_start();
        let Some(Pending::Options(packet)) = self.pending.as_mut() else {
            return false;
        };
        if let Some(entry) = parse_option_entry(trimmed) {
            packet.options.push(entry);
            self.option_path.clear();
            self.option_path.push((indent, packet.options.len() - 1));
            packet.records.push(mark(record, ParseStatus::Parsed));
            return true;
        }
        if let Some(child) = parse_sub_or_target(trimmed) {
            match child.kind {
                SubLineKind::SubOption => {
                    trim_option_path(&mut self.option_path, indent);
                    let Some(parent) = option_at_path_mut(&mut packet.options, &self.option_path)
                    else {
                        return false;
                    };
                    parent.sub_options.push(OptionEntry {
                        index: child.index,
                        option_type: "subOption".into(),
                        entity: child.entity,
                        entity_raw: child.entity_raw,
                        error: child.error,
                        availability: child.availability,
                        raw_error: child.raw_error,
                        error_param: child.error_param,
                        targets: Vec::new(),
                        target_entries: Vec::new(),
                        sub_options: Vec::new(),
                    });
                    self.option_path
                        .push((indent, parent.sub_options.len() - 1));
                }
                SubLineKind::Target => {
                    trim_option_path(&mut self.option_path, indent);
                    let Some(parent) = option_at_path_mut(&mut packet.options, &self.option_path)
                    else {
                        return false;
                    };
                    parent.target_entries.push(OptionTarget {
                        index: child.index,
                        entity: child.entity,
                        entity_raw: child.entity_raw,
                        error: child.error,
                        availability: child.availability,
                        raw_error: child.raw_error,
                        error_param: child.error_param,
                    });
                    parent.targets = project_target_entities(&parent.target_entries);
                }
            }
            packet.records.push(mark(record, ParseStatus::Parsed));
            return true;
        }
        false
    }

    fn attach_send_option(&mut self, pairs: &[(String, String)], record: RawLogRecord) {
        let Some(Pending::SendOption(packet)) = self.pending.as_mut() else {
            return;
        };
        if let Some(id) = find_pair(pairs, "id").and_then(|v| v.parse().ok()) {
            packet.id = id;
        }
        if let Some(v) = find_pair(pairs, "selectedOption").and_then(|v| v.parse().ok()) {
            packet.option = v;
        }
        if let Some(v) = find_pair(pairs, "selectedSubOption").and_then(|v| v.parse().ok()) {
            packet.sub_option = v;
        }
        if let Some(v) = find_pair(pairs, "selectedTarget").and_then(|v| v.parse().ok()) {
            packet.target = v;
        }
        if let Some(v) = find_pair(pairs, "selectedPosition").and_then(|v| v.parse().ok()) {
            packet.position = v;
        }
        packet.records.push(mark(record, ParseStatus::Parsed));
    }

    fn attach_choices(&mut self, trimmed: &str, record: RawLogRecord) {
        let Some(Pending::Choices(packet)) = self.pending.as_mut() else {
            return;
        };
        if let Some(raw) = trimmed.strip_prefix("Source=") {
            packet.source_raw = raw.trim().to_string();
            packet.source = parse_entity_id(&packet.source_raw);
        } else if let Some(raw) = entity_index_value(trimmed, "Entities[") {
            if let Some(eid) = parse_entity_id(raw) {
                packet.entities.push(eid);
            }
        }
        packet.records.push(mark(record, ParseStatus::Parsed));
    }

    fn attach_chosen(&mut self, trimmed: &str, record: RawLogRecord) {
        let Some(Pending::ChosenEntities(packet)) = self.pending.as_mut() else {
            return;
        };
        if let Some(raw) = trimmed.strip_prefix("Source=") {
            packet.source_raw = raw.trim().to_string();
            packet.source = parse_entity_id(&packet.source_raw);
        } else if let Some(raw) = entity_index_value(trimmed, "Entities[") {
            if let Some(eid) = parse_entity_id(raw) {
                packet.entities.push(eid);
            }
        }
        packet.records.push(mark(record, ParseStatus::Parsed));
    }

    fn attach_send_choices(&mut self, trimmed: &str, record: RawLogRecord) {
        let Some(Pending::SendChoices(packet)) = self.pending.as_mut() else {
            return;
        };
        if let Some(raw) = trimmed.strip_prefix("Source=") {
            packet.source_raw = raw.trim().to_string();
            packet.source = parse_entity_id(&packet.source_raw);
        } else if let Some(raw) = entity_index_value(trimmed, "m_chosenEntities[") {
            if let Some(eid) = parse_entity_id(raw) {
                packet.entities.push(eid);
            }
        }
        packet.records.push(mark(record, ParseStatus::Parsed));
    }

    fn flush(&mut self) -> Vec<Packet> {
        let pending = self.pending.take();
        self.option_path.clear();
        match pending {
            None => Vec::new(),
            Some(Pending::SendOption(packet)) if !send_option_has_complete_selection(&packet) => {
                packet.records.into_iter().map(unknown).collect()
            }
            Some(pending) => vec![match pending {
                Pending::CreateGame(packet) => Packet::CreateGame(packet),
                Pending::FullEntity(packet) => Packet::FullEntity(packet),
                Pending::ShowEntity(packet) => Packet::ShowEntity(packet),
                Pending::ChangeEntity(packet) => Packet::ChangeEntity(packet),
                Pending::Options(packet) => Packet::Options(packet),
                Pending::Choices(packet) => Packet::Choices(packet),
                Pending::ChosenEntities(packet) => Packet::ChosenEntities(packet),
                Pending::SendOption(packet) => Packet::SendOption(packet),
                Pending::SendChoices(packet) => Packet::SendChoices(packet),
            }],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LineKind {
    CreateGame,
    FullEntity,
    ShowEntity,
    ChangeEntity,
    HideEntity,
    TagChange,
    BlockStart,
    BlockEnd,
    GameEntity,
    Player,
    Tag,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SubLineKind {
    SubOption,
    Target,
}

struct OptionChildLine {
    kind: SubLineKind,
    index: u32,
    entity: Option<EntityId>,
    entity_raw: String,
    error: Option<String>,
    availability: OptionAvailability,
    raw_error: Option<String>,
    error_param: Option<String>,
}

enum LineSource<'a> {
    Power(&'a str),
    Options(&'a str),
    SendOption(&'a str),
    EntityChoices(&'a str),
    EntitiesChosen(&'a str),
    SendChoices(&'a str),
    GameMeta(&'a str),
    Other,
}

const GAME_META_MARKER: &str = "GameState.DebugPrintGame() - ";

fn line_source(line: &str) -> LineSource<'_> {
    if let Some(idx) = line.find(POWER_MARKER) {
        return LineSource::Power(&line[idx + POWER_MARKER.len()..]);
    }
    if let Some(idx) = line.find(OPTIONS_MARKER) {
        return LineSource::Options(&line[idx + OPTIONS_MARKER.len()..]);
    }
    if let Some(idx) = line.find(SEND_OPTION_MARKER) {
        return LineSource::SendOption(&line[idx + SEND_OPTION_MARKER.len()..]);
    }
    if let Some(idx) = line.find(ENTITY_CHOICES_MARKER) {
        return LineSource::EntityChoices(&line[idx + ENTITY_CHOICES_MARKER.len()..]);
    }
    if let Some(idx) = line.find(ENTITIES_CHOSEN_MARKER) {
        return LineSource::EntitiesChosen(&line[idx + ENTITIES_CHOSEN_MARKER.len()..]);
    }
    if let Some(idx) = line.find(SEND_CHOICES_MARKER) {
        return LineSource::SendChoices(&line[idx + SEND_CHOICES_MARKER.len()..]);
    }
    if let Some(idx) = line.find(GAME_META_MARKER) {
        return LineSource::GameMeta(&line[idx + GAME_META_MARKER.len()..]);
    }
    LineSource::Other
}

fn classify(trimmed: &str) -> LineKind {
    if trimmed.starts_with("CREATE_GAME") {
        LineKind::CreateGame
    } else if trimmed.starts_with("FULL_ENTITY") {
        LineKind::FullEntity
    } else if trimmed.starts_with("SHOW_ENTITY") {
        LineKind::ShowEntity
    } else if trimmed.starts_with("CHANGE_ENTITY") {
        LineKind::ChangeEntity
    } else if trimmed.starts_with("HIDE_ENTITY") {
        LineKind::HideEntity
    } else if trimmed.starts_with("TAG_CHANGE") {
        LineKind::TagChange
    } else if trimmed.starts_with("BLOCK_START") {
        LineKind::BlockStart
    } else if trimmed.starts_with("BLOCK_END") {
        LineKind::BlockEnd
    } else if trimmed.starts_with("GameEntity") {
        LineKind::GameEntity
    } else if trimmed.starts_with("Player ") || trimmed.starts_with("Player\t") {
        LineKind::Player
    } else if trimmed.starts_with("tag=") {
        LineKind::Tag
    } else {
        LineKind::Other
    }
}

fn is_toplevel_keyword(trimmed: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "CREATE_GAME",
        "FULL_ENTITY",
        "TAG_CHANGE",
        "SHOW_ENTITY",
        "CHANGE_ENTITY",
        "HIDE_ENTITY",
        "BLOCK_START",
        "BLOCK_END",
        "SUB_SPELL_START",
        "SUB_SPELL_END",
        "META_DATA",
        "OPTIONS",
        "CHOICES",
        "CHOSEN_ENTITIES",
        "SEND_OPTION",
        "SEND_CHOICES",
    ];
    KEYWORDS.iter().any(|kw| {
        trimmed.starts_with(kw)
            && trimmed
                .as_bytes()
                .get(kw.len())
                .is_none_or(|b| b.is_ascii_whitespace() || *b == b'-')
    })
}

fn unknown(record: RawLogRecord) -> Packet {
    Packet::Unknown(UnknownPacket {
        record: mark(record, ParseStatus::Unrecognized),
    })
}

fn mark(mut record: RawLogRecord, status: ParseStatus) -> RawLogRecord {
    record.parse_status = status;
    record
}

fn parse_full_entity_head(trimmed: &str) -> FullEntityPacket {
    let rest = strip_prefix_tokens(trimmed, &["FULL_ENTITY", "-", "Creating", "Updating"]);
    let pairs = parse_pairs(rest);
    let mut entity_id = EntityId(0);
    let mut card_id = None;
    for (key, value) in &pairs {
        if key.eq_ignore_ascii_case("ID") || key.eq_ignore_ascii_case("EntityID") {
            if let Ok(id) = value.parse() {
                entity_id = EntityId(id);
            }
        } else if key.eq_ignore_ascii_case("CardID") && !value.is_empty() {
            card_id = Some(CardId::new(value.as_str()));
        } else if looks_like_entity_ref(value) {
            entity_id = entity_id_from_raw(value);
        }
    }
    FullEntityPacket {
        entity_id,
        card_id,
        tags: Vec::new(),
        records: Vec::new(),
    }
}

fn parse_show_entity_head(trimmed: &str) -> ShowEntityPacket {
    let rest = strip_prefix_tokens(trimmed, &["SHOW_ENTITY", "-", "Updating"]);
    let pairs = parse_pairs(rest);
    let entity_raw = find_pair(&pairs, "Entity").unwrap_or_default();
    let card = find_pair(&pairs, "CardID").unwrap_or_default();
    ShowEntityPacket {
        entity_id: parse_entity_id(&entity_raw),
        entity_raw,
        card_id: nonempty_card(&card),
        tags: Vec::new(),
        records: Vec::new(),
    }
}

fn parse_change_entity_head(trimmed: &str) -> ChangeEntityPacket {
    let rest = strip_prefix_tokens(trimmed, &["CHANGE_ENTITY", "-", "Updating"]);
    let pairs = parse_pairs(rest);
    let entity_raw = find_pair(&pairs, "Entity").unwrap_or_default();
    let card = find_pair(&pairs, "CardID").unwrap_or_default();
    ChangeEntityPacket {
        entity_id: parse_entity_id(&entity_raw),
        entity_raw,
        card_id: nonempty_card(&card),
        tags: Vec::new(),
        records: Vec::new(),
    }
}

fn parse_hide_entity(record: RawLogRecord, trimmed: &str) -> HideEntityPacket {
    let rest = strip_prefix_tokens(trimmed, &["HIDE_ENTITY", "-"]);
    let pairs = parse_pairs(rest);
    let entity_raw = find_pair(&pairs, "Entity").unwrap_or_default();
    let tag_name = find_pair(&pairs, "tag").unwrap_or_default();
    let value_raw = find_pair(&pairs, "value").unwrap_or_default();
    let zone = if tag_name.eq_ignore_ascii_case("ZONE") {
        Some(game_tag::value_by_name("ZONE", &value_raw))
    } else {
        None
    };
    HideEntityPacket {
        entity_id: parse_entity_id(&entity_raw),
        entity_raw,
        zone,
        records: vec![mark(record, ParseStatus::Parsed)],
    }
}

fn parse_block_start(record: RawLogRecord, trimmed: &str) -> BlockStartPacket {
    let rest = strip_prefix_tokens(trimmed, &["BLOCK_START"]);
    let pairs = parse_pairs(rest);
    let source_raw = find_pair(&pairs, "Entity").unwrap_or_default();
    let target_raw = find_pair(&pairs, "Target").unwrap_or_default();
    let effect = find_pair(&pairs, "EffectCardId").unwrap_or_default();
    BlockStartPacket {
        block_type: find_pair(&pairs, "BlockType").unwrap_or_default(),
        source: parse_entity_id(&source_raw),
        source_raw,
        target: parse_entity_id(&target_raw),
        target_raw,
        effect_card_id: nonempty_card(&effect),
        effect_index: find_pair(&pairs, "EffectIndex").and_then(|v| v.parse().ok()),
        sub_option: find_pair(&pairs, "SubOption").and_then(|v| v.parse().ok()),
        trigger_keyword: find_pair(&pairs, "TriggerKeyword").filter(|v| !v.is_empty()),
        records: vec![mark(record, ParseStatus::Parsed)],
    }
}

fn parse_tag_change(record: RawLogRecord, trimmed: &str) -> TagChangePacket {
    let rest = strip_prefix_tokens(trimmed, &["TAG_CHANGE"]);
    let pairs = parse_pairs(rest);
    let entity_raw = find_pair(&pairs, "Entity").unwrap_or_default();
    let tag_name = find_pair(&pairs, "tag").unwrap_or_default();
    let value_raw = find_pair(&pairs, "value").unwrap_or_default();
    let entity = entity_id_from_raw(&entity_raw);
    let tag = game_tag::id_by_name(&tag_name);
    let value = game_tag::value_by_name(&tag_name, &value_raw);
    TagChangePacket {
        entity,
        tag,
        value,
        tag_name,
        value_raw,
        entity_raw,
        record: Some(mark(record, ParseStatus::Parsed)),
    }
}

/// `GameState.DebugPrintGame()` 单行 → GameMetadataPacket。
fn parse_game_metadata(record: RawLogRecord, payload: &str) -> GameMetadataPacket {
    let pairs = parse_pairs(payload.trim_start());
    let field =
        |key: &str| find_pair(&pairs, key).map(|v| v.trim_end_matches(',').trim().to_string());
    GameMetadataPacket {
        build_number: field("BuildNumber").and_then(|v| v.parse().ok()),
        game_type: field("GameType"),
        format_type: field("FormatType"),
        scenario_id: field("ScenarioID").and_then(|v| v.parse().ok()),
        player_id: field("PlayerID").and_then(|v| v.parse().ok()),
        player_name: field("PlayerName"),
        records: vec![mark(record, ParseStatus::Parsed)],
    }
}

fn parse_game_entity(trimmed: &str) -> CreateGameEntity {
    let rest = strip_prefix_tokens(trimmed, &["GameEntity"]);
    let pairs = parse_pairs(rest);
    let entity_id = find_pair(&pairs, "EntityID")
        .and_then(|v| v.parse().ok())
        .map(EntityId)
        .unwrap_or(EntityId(0));
    CreateGameEntity {
        entity_id,
        tags: Vec::new(),
    }
}

fn parse_player(trimmed: &str) -> CreateGamePlayer {
    let rest = strip_prefix_tokens(trimmed, &["Player"]);
    let pairs = parse_pairs(rest);
    let entity_id = find_pair(&pairs, "EntityID")
        .and_then(|v| v.parse().ok())
        .map(EntityId)
        .unwrap_or(EntityId(0));
    let player_id = find_pair(&pairs, "PlayerID")
        .and_then(|v| v.parse().ok())
        .map(PlayerId)
        .unwrap_or(PlayerId(0));
    let (account_hi, account_lo) = find_pair(&pairs, "GameAccountId")
        .map(|raw| parse_account_id(&raw))
        .unwrap_or((None, None));
    CreateGamePlayer {
        entity_id,
        player_id,
        account_hi,
        account_lo,
        tags: Vec::new(),
    }
}

fn parse_account_id(raw: &str) -> (Option<u64>, Option<u64>) {
    let inner = strip_brackets(raw);
    let pairs = parse_pairs(inner);
    (
        find_pair(&pairs, "hi").and_then(|v| v.parse().ok()),
        find_pair(&pairs, "lo").and_then(|v| v.parse().ok()),
    )
}

fn parse_tag_line(trimmed: &str) -> Option<RawTag> {
    let pairs = parse_pairs(trimmed);
    let name = find_pair(&pairs, "tag")?;
    let value = find_pair(&pairs, "value").unwrap_or_default();
    Some(RawTag { name, value })
}

fn attach_create_game_tag(packet: &mut CreateGamePacket, tag: RawTag) {
    if let Some(player) = packet.players.last_mut() {
        player.tags.push(tag);
    } else if let Some(game) = packet.game_entity.as_mut() {
        game.tags.push(tag);
    } else {
        packet.game_entity = Some(CreateGameEntity {
            entity_id: EntityId(0),
            tags: vec![tag],
        });
    }
}

fn parse_options_head(trimmed: &str) -> OptionsPacket {
    let pairs = parse_pairs(trimmed);
    OptionsPacket {
        id: find_pair(&pairs, "id")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        options: Vec::new(),
        records: Vec::new(),
    }
}

fn parse_option_entry(trimmed: &str) -> Option<OptionEntry> {
    let rest = strip_indexed_prefix(trimmed, "option")?;
    let (index, rest) = rest?;
    let pairs = parse_pairs(rest);
    let (entity, entity_raw) = option_entity(&pairs);
    let option_type = find_pair(&pairs, "type").unwrap_or_default();
    let errors = option_error_fields(&pairs, &option_type, true);
    Some(OptionEntry {
        index,
        option_type,
        entity,
        entity_raw,
        error: errors.error,
        availability: errors.availability,
        raw_error: errors.raw_error,
        error_param: errors.error_param,
        targets: Vec::new(),
        target_entries: Vec::new(),
        sub_options: Vec::new(),
    })
}

fn parse_sub_or_target(trimmed: &str) -> Option<OptionChildLine> {
    let (kind, rest) = match strip_indexed_prefix(trimmed, "subOption") {
        Some(rest) => (SubLineKind::SubOption, rest?),
        None => {
            let rest = strip_indexed_prefix(trimmed, "target")?;
            (SubLineKind::Target, rest?)
        }
    };
    let (index, rest) = rest;
    let pairs = parse_pairs(rest);
    let (entity, entity_raw) = option_entity(&pairs);
    let errors = option_error_fields(&pairs, "", false);
    Some(OptionChildLine {
        kind,
        index,
        entity,
        entity_raw,
        error: errors.error,
        availability: errors.availability,
        raw_error: errors.raw_error,
        error_param: errors.error_param,
    })
}

fn option_entity(pairs: &[(String, String)]) -> (Option<EntityId>, String) {
    let raw = find_pair(pairs, "mainEntity")
        .or_else(|| find_pair(pairs, "entity"))
        .or_else(|| find_pair(pairs, "entityName"))
        .unwrap_or_default();
    (parse_entity_id(&raw), raw)
}

struct OptionErrorFields {
    error: Option<String>,
    availability: OptionAvailability,
    raw_error: Option<String>,
    error_param: Option<String>,
}

fn option_error_fields(
    pairs: &[(String, String)],
    option_type: &str,
    is_top_level: bool,
) -> OptionErrorFields {
    let raw_error = find_pair(pairs, "error");
    let error_param = find_pair(pairs, "errorParam");
    let availability = match raw_error.as_deref() {
        None | Some("") => OptionAvailability::LegacyUnspecified,
        Some(error) if error.eq_ignore_ascii_case("NONE") => OptionAvailability::Allowed,
        Some(error)
            if is_top_level
                && option_type.eq_ignore_ascii_case("END_TURN")
                && error.eq_ignore_ascii_case("INVALID") =>
        {
            OptionAvailability::Allowed
        }
        Some(_) => OptionAvailability::Blocked,
    };
    let error = raw_error
        .as_ref()
        .filter(|error| !error.is_empty() && !error.eq_ignore_ascii_case("NONE"))
        .cloned();
    OptionErrorFields {
        error,
        availability,
        raw_error,
        error_param,
    }
}

fn is_options_child(trimmed: &str) -> bool {
    trimmed.starts_with("option ")
        || trimmed.starts_with("subOption ")
        || trimmed.starts_with("target ")
}

fn parse_choices_head(trimmed: &str) -> ChoicesPacket {
    let pairs = parse_pairs(trimmed);
    let player_raw = find_pair(&pairs, "Player").unwrap_or_default();
    ChoicesPacket {
        id: find_pair(&pairs, "id")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        task_list: optional_u64_pair(&pairs, "TaskList"),
        player_entity: parse_entity_id(&player_raw),
        player_raw,
        choice_type: find_pair(&pairs, "ChoiceType").unwrap_or_default(),
        min: find_pair(&pairs, "CountMin")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        max: find_pair(&pairs, "CountMax")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        source: find_pair(&pairs, "Source").and_then(|raw| parse_entity_id(&raw)),
        source_raw: find_pair(&pairs, "Source").unwrap_or_default(),
        entities: Vec::new(),
        records: Vec::new(),
    }
}

fn is_choices_child(trimmed: &str) -> bool {
    trimmed.starts_with("Source=") || is_entities_index_line(trimmed)
}

fn is_entities_index_line(trimmed: &str) -> bool {
    trimmed.starts_with("Entities[")
}

fn is_chosen_child(trimmed: &str) -> bool {
    trimmed.starts_with("Source=") || is_entities_index_line(trimmed)
}

fn is_send_choices_child(trimmed: &str) -> bool {
    trimmed.starts_with("Source=") || trimmed.starts_with("m_chosenEntities[")
}

fn parse_chosen_head(trimmed: &str) -> ChosenEntitiesPacket {
    let pairs = parse_pairs(trimmed);
    let player_raw = find_pair(&pairs, "Player").unwrap_or_default();
    let source_raw = find_pair(&pairs, "Source").unwrap_or_default();
    ChosenEntitiesPacket {
        id: find_pair(&pairs, "id")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        task_list: optional_u64_pair(&pairs, "TaskList"),
        player_entity: parse_entity_id(&player_raw),
        player_raw,
        source: parse_entity_id(&source_raw),
        source_raw,
        entities: Vec::new(),
        records: Vec::new(),
    }
}

fn parse_send_choices_head(trimmed: &str) -> SendChoicesPacket {
    let pairs = parse_pairs(trimmed);
    let player_raw = find_pair(&pairs, "Player").unwrap_or_default();
    let source_raw = find_pair(&pairs, "Source").unwrap_or_default();
    SendChoicesPacket {
        id: find_pair(&pairs, "id")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        task_list: optional_u64_pair(&pairs, "TaskList"),
        player_entity: parse_entity_id(&player_raw),
        player_raw,
        choice_type: find_pair(&pairs, "ChoiceType").unwrap_or_default(),
        source: parse_entity_id(&source_raw),
        source_raw,
        entities: Vec::new(),
        records: Vec::new(),
    }
}

fn send_option_has_selection(packet: &SendOptionPacket) -> bool {
    send_option_has_field(packet, "selectedOption")
}

fn send_option_has_complete_selection(packet: &SendOptionPacket) -> bool {
    [
        "selectedOption",
        "selectedSubOption",
        "selectedTarget",
        "selectedPosition",
    ]
    .into_iter()
    .all(|field| send_option_has_field(packet, field))
}

fn send_option_has_field(packet: &SendOptionPacket, key: &str) -> bool {
    packet.records.iter().any(|record| {
        let LineSource::SendOption(payload) = line_source(&record.raw_line) else {
            return false;
        };
        find_pair(&parse_pairs(payload), key).is_some()
    })
}

fn send_option_numeric_fields_are_valid(pairs: &[(String, String)]) -> bool {
    let valid_u64 = |key| find_pair(pairs, key).is_none_or(|value| value.parse::<u64>().is_ok());
    let valid_i32 = |key| find_pair(pairs, key).is_none_or(|value| value.parse::<i32>().is_ok());
    valid_u64("id")
        && [
            "selectedOption",
            "selectedSubOption",
            "selectedTarget",
            "selectedPosition",
        ]
        .into_iter()
        .all(valid_i32)
}

fn optional_u64_pair(pairs: &[(String, String)], key: &str) -> Option<u64> {
    find_pair(pairs, key).and_then(|value| value.parse().ok())
}

fn leading_indent(input: &str) -> usize {
    input
        .chars()
        .take_while(|ch| ch.is_ascii_whitespace())
        .map(|ch| if ch == '\t' { 4 } else { 1 })
        .sum()
}

fn trim_option_path(path: &mut Vec<(usize, usize)>, child_indent: usize) {
    while path
        .last()
        .is_some_and(|(entry_indent, _)| *entry_indent >= child_indent)
    {
        path.pop();
    }
}

fn option_at_path_mut<'a>(
    options: &'a mut [OptionEntry],
    path: &[(usize, usize)],
) -> Option<&'a mut OptionEntry> {
    let (_, first) = *path.first()?;
    let mut entry = options.get_mut(first)?;
    for (_, index) in &path[1..] {
        entry = entry.sub_options.get_mut(*index)?;
    }
    Some(entry)
}

fn project_target_entities(targets: &[OptionTarget]) -> Vec<EntityId> {
    targets.iter().filter_map(|target| target.entity).collect()
}

fn entity_index_value<'a>(trimmed: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = trimmed.strip_prefix(prefix)?;
    let (_, value) = rest.split_once("]=")?;
    Some(value)
}

fn strip_indexed_prefix<'a>(trimmed: &'a str, kind: &str) -> Option<Option<(u32, &'a str)>> {
    let rest = trimmed.trim_start().strip_prefix(kind)?;
    if !rest.is_empty() && !rest.starts_with(|c: char| c.is_ascii_whitespace()) {
        return None;
    }
    let rest = rest.trim_start();
    let (idx_s, rest) = match rest.split_once(|c: char| c.is_ascii_whitespace()) {
        Some(parts) => parts,
        None => return Some(None),
    };
    let Ok(index) = idx_s.parse() else {
        return Some(None);
    };
    Some(Some((index, rest)))
}

fn nonempty_card(raw: &str) -> Option<CardId> {
    if raw.is_empty() {
        None
    } else {
        Some(CardId::new(raw))
    }
}

fn strip_prefix_tokens<'a>(input: &'a str, tokens: &[&str]) -> &'a str {
    let mut rest = input.trim_start();
    for token in tokens {
        let trimmed = rest.trim_start();
        if let Some(after) = trimmed.strip_prefix(token) {
            if after.is_empty() || after.starts_with(|c: char| c.is_ascii_whitespace() || c == '-')
            {
                rest = after;
                continue;
            }
        }
    }
    rest.trim_start()
}

fn find_pair(pairs: &[(String, String)], key: &str) -> Option<String> {
    pairs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
}

fn looks_like_entity_ref(value: &str) -> bool {
    value.starts_with('[') && value.contains("id=")
}

fn strip_brackets(raw: &str) -> &str {
    raw.strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(raw)
}

/// 三种实体引用：纯数字、`GameEntity`、`[entityName=... id=N ...]`。
/// 解析失败（玩家名等）返回 `None`，由调用方保留原文。
fn parse_entity_id(raw: &str) -> Option<EntityId> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "0" || raw == "-1" {
        return None;
    }
    if raw == "GameEntity" {
        return Some(EntityId(1));
    }
    if let Ok(id) = raw.parse::<i32>() {
        return (id != 0).then_some(EntityId(id));
    }
    if raw.starts_with('[') {
        let inner = strip_brackets(raw);
        for (key, value) in parse_pairs(inner) {
            if key.eq_ignore_ascii_case("id") || key.eq_ignore_ascii_case("EntityID") {
                if let Ok(id) = value.parse::<i32>() {
                    if id != 0 {
                        return Some(EntityId(id));
                    }
                }
            }
        }
    }
    None
}

fn entity_id_from_raw(raw: &str) -> EntityId {
    parse_entity_id(raw).unwrap_or(EntityId(0))
}

/// `key=value` 扫描；`[...]` 值按括号深度匹配，以支持嵌套 entity 描述。
/// 非括号值读到下一个 `key=`（空白 + 标识 + `=`）为止，以保留玩家名中的空格。
fn parse_pairs(input: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            i = i.saturating_add(1);
            continue;
        }
        let key = input[key_start..i].to_string();
        i += 1;
        let (value, next) = parse_value(input, i);
        i = next;
        out.push((key, value));
    }
    out
}

fn parse_value(input: &str, start: usize) -> (String, usize) {
    let bytes = input.as_bytes();
    if start < bytes.len() && bytes[start] == b'[' {
        let mut depth = 0;
        let mut i = start;
        while i < bytes.len() {
            match bytes[i] {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return (input[start..=i].to_string(), i + 1);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        return (input[start..].to_string(), bytes.len());
    }
    let mut i = start;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() && next_is_key(bytes, i) {
            return (input[start..i].to_string(), i);
        }
        i += 1;
    }
    // 真实日志行尾常带空格（`value=WON `），必须去掉，
    // 否则 value_by_name 匹配失败导致 STEP/PLAYSTATE 全部归 0。
    (input[start..].trim_end().to_string(), bytes.len())
}

fn next_is_key(bytes: &[u8], whitespace_at: usize) -> bool {
    let mut j = whitespace_at;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    let key_start = j;
    while j < bytes.len() && bytes[j] != b'=' && !bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    j < bytes.len() && bytes[j] == b'=' && j > key_start
}

#[cfg(test)]
mod tests {
    use super::*;
    use hs_types::{LogFileId, LogSessionId};
    use std::sync::Arc;

    fn rec(line: &str, offset: u64) -> RawLogRecord {
        RawLogRecord {
            session_id: LogSessionId(1),
            file_id: LogFileId(1),
            byte_offset: offset,
            timestamp: None,
            raw_line: Arc::from(line),
            parse_status: ParseStatus::Pending,
        }
    }

    fn power(payload: &str) -> String {
        format!("D 21:35:47.0000000 GameState.DebugPrintPower() - {payload}")
    }

    fn options(payload: &str) -> String {
        format!("D 21:35:47.0000000 GameState.DebugPrintOptions() - {payload}")
    }

    fn send_option(payload: &str) -> String {
        format!("D 21:35:47.0000000 GameState.SendOption() - {payload}")
    }

    fn choices(payload: &str) -> String {
        format!("D 21:35:47.0000000 GameState.DebugPrintEntityChoices() - {payload}")
    }

    fn chosen(payload: &str) -> String {
        format!("D 21:35:47.0000000 GameState.DebugPrintEntitiesChosen() - {payload}")
    }

    fn send_choices(payload: &str) -> String {
        format!("D 21:35:47.0000000 GameState.SendChoices() - {payload}")
    }

    fn parse_lines(lines: &[String]) -> Vec<Packet> {
        let records: Vec<_> = lines
            .iter()
            .enumerate()
            .map(|(i, line)| rec(line, i as u64))
            .collect();
        PowerLogParser::parse_records(records)
    }

    #[test]
    fn parser_accepts_tailer_record_reexport_without_conversion() {
        let record: hs_log_tailer::RawLogRecord = RawLogRecord {
            session_id: LogSessionId(1),
            file_id: LogFileId(1),
            byte_offset: 0,
            timestamp: None,
            raw_line: Arc::from("unrecognized line"),
            parse_status: hs_log_tailer::ParseStatus::Pending,
        };

        let packets = PowerLogParser::parse_records([record]);
        let Packet::Unknown(unknown) = &packets[0] else {
            panic!("expected unknown packet");
        };
        assert_eq!(unknown.record.parse_status, ParseStatus::Unrecognized);
    }

    #[test]
    fn assembles_create_game_and_full_entity_then_tag_change() {
        let lines = [
            power("CREATE_GAME"),
            power("    GameEntity EntityID=1"),
            power("        tag=ZONE value=PLAY"),
            power("    Player EntityID=2 PlayerID=1 GameAccountId=[hi=1 lo=2]"),
            power("        tag=PLAYSTATE value=PLAYING"),
            power("FULL_ENTITY - Creating ID=9 CardID=CS2_029"),
            power("    tag=COST value=4"),
            power("TAG_CHANGE Entity=GameEntity tag=STEP value=BEGIN_MULLIGAN"),
        ];
        let packets = parse_lines(&lines);
        assert_eq!(packets.len(), 3);

        let Packet::CreateGame(create) = &packets[0] else {
            panic!("expected CreateGame");
        };
        assert_eq!(create.game_entity.as_ref().unwrap().entity_id, EntityId(1));
        assert_eq!(create.game_entity.as_ref().unwrap().tags[0].name, "ZONE");
        assert_eq!(create.players[0].player_id, PlayerId(1));
        assert_eq!(create.players[0].account_hi, Some(1));
        assert_eq!(create.players[0].account_lo, Some(2));
        assert_eq!(create.records.len(), 5);

        let Packet::FullEntity(full) = &packets[1] else {
            panic!("expected FullEntity");
        };
        assert_eq!(full.entity_id, EntityId(9));
        assert_eq!(full.card_id.as_ref().unwrap().as_str(), "CS2_029");
        assert_eq!(
            full.tags,
            vec![RawTag {
                name: "COST".into(),
                value: "4".into(),
            }]
        );
        assert_eq!(full.records.len(), 2);

        let Packet::TagChange(change) = &packets[2] else {
            panic!("expected TagChange");
        };
        assert_eq!(change.entity, EntityId(1));
        assert_eq!(change.tag_name, "STEP");
        assert_eq!(change.value_raw, "BEGIN_MULLIGAN");
        assert_eq!(change.tag, 19);
        assert_eq!(change.value, 4);
    }

    #[test]
    fn finish_emits_open_multiline_packet() {
        let mut parser = PowerLogParser::new();
        assert!(parser
            .push(rec(&power("FULL_ENTITY - Creating ID=4 CardID=HERO_01"), 0))
            .is_empty());
        let done = parser.finish();
        assert_eq!(done.len(), 1);
        assert!(matches!(&done[0], Packet::FullEntity(p) if p.entity_id == EntityId(4)));
    }

    #[test]
    fn unknown_lines_keep_raw_record() {
        let line = "I 21:35:47.0 LoadingScreen.OnSceneLoaded() - nextMode=GAMEPLAY";
        let packets = PowerLogParser::parse_records([rec(line, 10)]);
        let Packet::Unknown(unknown) = &packets[0] else {
            panic!("expected Unknown");
        };
        assert_eq!(unknown.record.raw_line.as_ref(), line);
        assert_eq!(unknown.record.byte_offset, 10);
        assert_eq!(unknown.record.parse_status, ParseStatus::Unrecognized);
    }

    #[test]
    fn nested_entity_ref_yields_id() {
        let line = power(
            "TAG_CHANGE Entity=[entityName=UNKNOWN ENTITY [cardType=INVALID] id=4 zone=PLAY zonePos=0 cardId= player=1] tag=DAMAGE value=0",
        );
        let packets = parse_lines(&[line]);
        let Packet::TagChange(change) = &packets[0] else {
            panic!("expected TagChange");
        };
        assert_eq!(change.entity, EntityId(4));
        assert_eq!(change.tag_name, "DAMAGE");
        assert_eq!(change.value, 0);
    }

    #[test]
    fn parse_pairs_handles_nested_brackets() {
        let pairs = parse_pairs(
            "Entity=[entityName=UNKNOWN ENTITY [cardType=INVALID] id=4] tag=DAMAGE value=0",
        );
        assert_eq!(pairs[0].0, "Entity");
        assert!(pairs[0].1.contains("id=4"));
        assert_eq!(pairs[1], ("tag".into(), "DAMAGE".into()));
        assert_eq!(pairs[2], ("value".into(), "0".into()));
    }

    #[test]
    fn parse_pairs_keeps_spaced_player_name() {
        let pairs = parse_pairs("Entity=UNKNOWN HUMAN PLAYER tag=PLAYSTATE value=WON");
        assert_eq!(pairs[0], ("Entity".into(), "UNKNOWN HUMAN PLAYER".into()));
        assert_eq!(pairs[1], ("tag".into(), "PLAYSTATE".into()));
        assert_eq!(pairs[2], ("value".into(), "WON".into()));
    }

    #[test]
    fn parses_block_start_attack_and_block_end() {
        let lines = [
            power(
                "BLOCK_START BlockType=ATTACK Entity=[entityName=CS2_029 id=9 zone=PLAY zonePos=1 cardId=CS2_029 player=1] EffectCardId= EffectIndex=-1 Target=[entityName=HERO_02 id=6 zone=PLAY zonePos=0 cardId=HERO_02 player=2] SubOption=-1",
            ),
            power("BLOCK_END"),
        ];
        let packets = parse_lines(&lines);
        assert_eq!(packets.len(), 2);
        let Packet::BlockStart(start) = &packets[0] else {
            panic!("expected BlockStart");
        };
        assert_eq!(start.block_type, "ATTACK");
        assert_eq!(start.source, Some(EntityId(9)));
        assert!(start.source_raw.contains("id=9"));
        assert_eq!(start.target, Some(EntityId(6)));
        assert!(start.effect_card_id.is_none());
        assert_eq!(start.effect_index, Some(-1));
        assert_eq!(start.sub_option, Some(-1));
        assert!(matches!(&packets[1], Packet::BlockEnd(_)));
    }

    #[test]
    fn parses_block_start_trigger_game_entity() {
        let line = power(
            "BLOCK_START BlockType=TRIGGER Entity=GameEntity EffectCardId=System EffectIndex=-1 Target=0 SubOption=-1 TriggerKeyword=TRIGGER_VISUAL",
        );
        let packets = parse_lines(&[line]);
        let Packet::BlockStart(start) = &packets[0] else {
            panic!("expected BlockStart");
        };
        assert_eq!(start.block_type, "TRIGGER");
        assert_eq!(start.source, Some(EntityId(1)));
        assert_eq!(start.source_raw, "GameEntity");
        assert_eq!(start.target, None);
        assert_eq!(start.target_raw, "0");
        assert_eq!(
            start.effect_card_id.as_ref().map(|c| c.as_str()),
            Some("System")
        );
        assert_eq!(start.trigger_keyword.as_deref(), Some("TRIGGER_VISUAL"));
    }

    #[test]
    fn block_start_keeps_unparsed_player_name() {
        let line = power("BLOCK_START BlockType=PLAY Entity=Anduin EffectCardId=CS2_029 Target=");
        let packets = parse_lines(&[line]);
        let Packet::BlockStart(start) = &packets[0] else {
            panic!("expected BlockStart");
        };
        assert_eq!(start.source, None);
        assert_eq!(start.source_raw, "Anduin");
        assert_eq!(start.target, None);
    }

    #[test]
    fn parses_show_entity_with_tags() {
        let lines = [
            power(
                "SHOW_ENTITY - Updating Entity=[entityName=UNKNOWN ENTITY [cardType=INVALID] id=12 zone=HAND zonePos=1 cardId= player=1] CardID=CS2_029",
            ),
            power("    tag=COST value=4"),
            power("    tag=ZONE value=HAND"),
        ];
        let packets = parse_lines(&lines);
        let Packet::ShowEntity(show) = &packets[0] else {
            panic!("expected ShowEntity");
        };
        assert_eq!(show.entity_id, Some(EntityId(12)));
        assert!(show.entity_raw.contains("id=12"));
        assert_eq!(show.card_id.as_ref().unwrap().as_str(), "CS2_029");
        assert_eq!(show.tags.len(), 2);
        assert_eq!(show.tags[0].name, "COST");
        assert_eq!(show.records.len(), 3);
    }

    #[test]
    fn parses_change_entity_with_tags() {
        let lines = [
            power("CHANGE_ENTITY - Updating Entity=9 CardID=NEW1_026"),
            power("    tag=ATK value=3"),
        ];
        let packets = parse_lines(&lines);
        let Packet::ChangeEntity(change) = &packets[0] else {
            panic!("expected ChangeEntity");
        };
        assert_eq!(change.entity_id, Some(EntityId(9)));
        assert_eq!(change.entity_raw, "9");
        assert_eq!(change.card_id.as_ref().unwrap().as_str(), "NEW1_026");
        assert_eq!(change.tags[0].value, "3");
    }

    #[test]
    fn parses_hide_entity_zone_deck() {
        let line = power(
            "HIDE_ENTITY - Entity=[entityName=CS2_029 id=12 zone=HAND zonePos=1 cardId=CS2_029 player=1] tag=ZONE value=DECK",
        );
        let packets = parse_lines(&[line]);
        let Packet::HideEntity(hide) = &packets[0] else {
            panic!("expected HideEntity");
        };
        assert_eq!(hide.entity_id, Some(EntityId(12)));
        assert!(hide.entity_raw.contains("id=12"));
        assert_eq!(hide.zone, Some(game_tag::zone::DECK));
    }

    #[test]
    fn show_hide_change_keep_unparsed_player_name() {
        let show_line = power("SHOW_ENTITY - Updating Entity=Anduin CardID=CS2_029");
        let Packet::ShowEntity(show) = &parse_lines(&[show_line])[0] else {
            panic!("expected ShowEntity");
        };
        assert_eq!(show.entity_id, None);
        assert_eq!(show.entity_raw, "Anduin");
        assert_eq!(show.card_id.as_ref().unwrap().as_str(), "CS2_029");

        let change_line = power("CHANGE_ENTITY - Updating Entity=Anduin CardID=NEW1_026");
        let Packet::ChangeEntity(change) = &parse_lines(&[change_line])[0] else {
            panic!("expected ChangeEntity");
        };
        assert_eq!(change.entity_id, None);
        assert_eq!(change.entity_raw, "Anduin");

        let hide_line = power("HIDE_ENTITY - Entity=Anduin tag=ZONE value=DECK");
        let Packet::HideEntity(hide) = &parse_lines(&[hide_line])[0] else {
            panic!("expected HideEntity");
        };
        assert_eq!(hide.entity_id, None);
        assert_eq!(hide.entity_raw, "Anduin");
        assert_eq!(hide.zone, Some(game_tag::zone::DECK));
    }

    #[test]
    fn parses_options_with_target_and_suboption() {
        let lines = [
            options("id=2"),
            options("  option 0 type=END_TURN mainEntity= error=INVALID errorParam="),
            options(
                "  option 1 type=POWER mainEntity=[entityName=HERO_01 id=4 zone=PLAY zonePos=0 cardId=HERO_01 player=1] error=NONE errorParam=",
            ),
            options(
                "    target 0 entity=[entityName=HERO_02 id=6 zone=PLAY zonePos=0 cardId=HERO_02 player=2] error=NONE errorParam=",
            ),
            options(
                "    target 1 entity=[entityName=HERO_03 id=7 zone=PLAY zonePos=0 cardId=HERO_03 player=2] error=REQ_FRIENDLY_TARGET errorParam=42",
            ),
            options(
                "  option 2 type=POWER mainEntity=[entityName=CS2_034 id=8 zone=PLAY cardId=CS2_034 player=1] error=NONE errorParam=",
            ),
            options(
                "    subOption 0 entity=[entityName=CS2_034 id=8 zone=PLAY cardId=CS2_034 player=1] error=NONE errorParam=",
            ),
            options(
                "      target 0 entity=[entityName=CS2_029 id=9 zone=PLAY cardId=CS2_029 player=1] error=NONE errorParam=",
            ),
        ];
        let packets = parse_lines(&lines);
        let Packet::Options(opts) = &packets[0] else {
            panic!("expected Options");
        };
        assert_eq!(opts.id, 2);
        assert_eq!(opts.options.len(), 3);
        assert_eq!(opts.options[0].option_type, "END_TURN");
        assert_eq!(opts.options[0].entity, None);
        assert_eq!(opts.options[0].error.as_deref(), Some("INVALID"));
        assert_eq!(opts.options[0].availability, OptionAvailability::Allowed);
        assert_eq!(opts.options[0].raw_error.as_deref(), Some("INVALID"));
        assert_eq!(opts.options[0].error_param.as_deref(), Some(""));
        assert_eq!(opts.options[1].entity, Some(EntityId(4)));
        assert!(opts.options[1].error.is_none());
        assert_eq!(opts.options[1].availability, OptionAvailability::Allowed);
        assert_eq!(opts.options[1].raw_error.as_deref(), Some("NONE"));
        assert_eq!(opts.options[1].targets, vec![EntityId(6), EntityId(7)]);
        assert_eq!(opts.options[1].target_entries.len(), 2);
        assert_eq!(opts.options[1].target_entries[0].index, 0);
        assert_eq!(opts.options[1].target_entries[0].entity, Some(EntityId(6)));
        assert!(opts.options[1].target_entries[0]
            .entity_raw
            .contains("id=6"));
        assert_eq!(
            opts.options[1].target_entries[0].availability,
            OptionAvailability::Allowed
        );
        assert_eq!(
            opts.options[1].target_entries[1].availability,
            OptionAvailability::Blocked
        );
        assert_eq!(
            opts.options[1].target_entries[1].raw_error.as_deref(),
            Some("REQ_FRIENDLY_TARGET")
        );
        assert_eq!(
            opts.options[1].target_entries[1].error_param.as_deref(),
            Some("42")
        );
        assert_eq!(opts.options[2].sub_options.len(), 1);
        assert_eq!(opts.options[2].sub_options[0].entity, Some(EntityId(8)));
        assert_eq!(opts.options[2].sub_options[0].targets, vec![EntityId(9)]);
        assert_eq!(opts.records.len(), 8);
    }

    #[test]
    fn parses_legacy_and_recursive_sub_options_without_loss() {
        let lines = [
            options("id=7"),
            options("  option 0 type=POWER mainEntity=17"),
            options("  option 1 type=POWER mainEntity=22 error= errorParam="),
            options("    subOption 2 entity=18 error=NONE errorParam="),
            options("      subOption 4 entity=19 error=REQ_ENOUGH_MANA errorParam=3"),
            options("        target 9 entity=20 error=NONE errorParam="),
            options("    target 6 entity=21 error=NONE errorParam="),
        ];
        let packets = parse_lines(&lines);
        let Packet::Options(options) = &packets[0] else {
            panic!("expected Options");
        };
        let top = &options.options[0];
        assert_eq!(top.entity_raw, "17");
        assert_eq!(top.availability, OptionAvailability::LegacyUnspecified);
        assert_eq!(top.raw_error, None);
        assert_eq!(top.error_param, None);

        let explicit_empty = &options.options[1];
        assert_eq!(
            explicit_empty.availability,
            OptionAvailability::LegacyUnspecified
        );
        assert_eq!(explicit_empty.raw_error.as_deref(), Some(""));
        assert_eq!(explicit_empty.error_param.as_deref(), Some(""));
        assert_eq!(explicit_empty.targets, vec![EntityId(21)]);
        assert_eq!(explicit_empty.target_entries[0].index, 6);

        let sub = &explicit_empty.sub_options[0];
        assert_eq!(sub.index, 2);
        assert_eq!(sub.availability, OptionAvailability::Allowed);
        let nested = &sub.sub_options[0];
        assert_eq!(nested.index, 4);
        assert_eq!(nested.entity_raw, "19");
        assert_eq!(nested.availability, OptionAvailability::Blocked);
        assert_eq!(nested.error_param.as_deref(), Some("3"));
        assert_eq!(nested.targets, vec![EntityId(20)]);
        assert_eq!(nested.target_entries[0].index, 9);
    }

    #[test]
    fn decision_flush_commits_only_complete_pending_decision_once() {
        let mut parser = PowerLogParser::new();
        assert!(parser.push(rec(&options("id=12"), 0)).is_empty());
        assert!(parser
            .push(rec(
                &options("  option 0 type=POWER mainEntity=4 error=NONE errorParam="),
                1,
            ))
            .is_empty());
        assert!(parser
            .push(rec(
                &options("    subOption 0 entity=5 error=NONE errorParam="),
                2,
            ))
            .is_empty());
        assert!(parser
            .push(rec(
                &options("      target 3 entity=6 error=NONE errorParam="),
                3,
            ))
            .is_empty());
        assert!(parser.has_pending_decision());

        let flushed = parser.flush_pending_decision();
        assert_eq!(flushed.len(), 1);
        let Packet::Options(options) = &flushed[0] else {
            panic!("expected Options");
        };
        assert_eq!(options.id, 12);
        assert_eq!(options.records.len(), 4);
        assert_eq!(options.options[0].sub_options[0].targets, vec![EntityId(6)]);
        assert!(!parser.has_pending_decision());
        assert!(parser.flush_pending_decision().is_empty());

        assert!(parser
            .push(rec(&power("FULL_ENTITY - Creating ID=9 CardID=CS2_029"), 4))
            .is_empty());
        assert!(!parser.has_pending_decision());
        assert!(parser.flush_pending_decision().is_empty());
        assert!(parser
            .push(rec(&power("    tag=ZONE value=HAND"), 5))
            .is_empty());
        let finished = parser.finish();
        let Packet::FullEntity(entity) = &finished[0] else {
            panic!("expected FullEntity");
        };
        assert_eq!(entity.tags.len(), 1);
    }

    #[test]
    fn decision_flush_commits_pending_choices() {
        let mut parser = PowerLogParser::new();
        assert!(parser
            .push(rec(
                &choices("id=3 Player=2 TaskList=17 ChoiceType=GENERAL CountMin=1 CountMax=1"),
                0,
            ))
            .is_empty());
        assert!(parser
            .push(rec(&choices("  Source=GameEntity"), 1))
            .is_empty());
        assert!(parser.push(rec(&choices("  Entities[0]=12"), 2)).is_empty());
        assert!(parser.has_pending_decision());

        let flushed = parser.flush_pending_decision();
        let Packet::Choices(choices) = &flushed[0] else {
            panic!("expected Choices");
        };
        assert_eq!(choices.task_list, Some(17));
        assert_eq!(choices.source_raw, "GameEntity");
        assert_eq!(choices.entities, vec![EntityId(12)]);
        assert!(parser.flush_pending_decision().is_empty());
    }

    #[test]
    fn late_or_orphan_decision_children_are_unknown() {
        let mut parser = PowerLogParser::new();
        assert!(parser.push(rec(&options("id=3"), 0)).is_empty());
        assert!(parser
            .push(rec(&options("  option 0 type=END_TURN mainEntity="), 1))
            .is_empty());
        assert_eq!(parser.flush_pending_decision().len(), 1);

        let late = parser.push(rec(
            &options("    target 0 entity=6 error=NONE errorParam="),
            2,
        ));
        assert_eq!(late.len(), 1);
        assert!(matches!(late[0], Packet::Unknown(_)));
        assert!(!parser.has_pending_decision());

        let orphan_choice = parser.push(rec(&choices("  Entities[0]=12"), 3));
        assert_eq!(orphan_choice.len(), 1);
        assert!(matches!(orphan_choice[0], Packet::Unknown(_)));
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn parses_send_option_single_line() {
        let line = send_option(
            "selectedOption=1 selectedSubOption=-1 selectedTarget=0 selectedPosition=0",
        );
        let packets = parse_lines(&[line]);
        let Packet::SendOption(send) = &packets[0] else {
            panic!("expected SendOption");
        };
        assert_eq!(send.id, 0);
        assert_eq!(send.option, 1);
        assert_eq!(send.sub_option, -1);
        assert_eq!(send.target, 0);
        assert_eq!(send.position, 0);
    }

    #[test]
    fn complete_send_option_is_emitted_by_same_push() {
        let mut parser = PowerLogParser::new();
        let packets = parser.push(rec(
            &send_option(
                "id=14 selectedOption=1 selectedSubOption=2 selectedTarget=6 selectedPosition=3",
            ),
            0,
        ));
        assert_eq!(packets.len(), 1);
        let Packet::SendOption(send) = &packets[0] else {
            panic!("expected SendOption");
        };
        assert_eq!((send.id, send.option, send.sub_option), (14, 1, 2));
        assert_eq!((send.target, send.position), (6, 3));
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn parses_send_option_scattered_fields() {
        let lines = [
            send_option("id=3"),
            send_option("selectedOption=2"),
            send_option("selectedSubOption=-1"),
            send_option("selectedTarget=6"),
            send_option("selectedPosition=0"),
        ];
        let packets = parse_lines(&lines);
        assert_eq!(packets.len(), 1);
        let Packet::SendOption(send) = &packets[0] else {
            panic!("expected SendOption");
        };
        assert_eq!(send.id, 3);
        assert_eq!(send.option, 2);
        assert_eq!(send.sub_option, -1);
        assert_eq!(send.target, 6);
        assert_eq!(send.position, 0);
        assert_eq!(send.records.len(), 5);
    }

    #[test]
    fn invalid_send_option_number_never_becomes_a_default_selection() {
        let mut parser = PowerLogParser::new();
        assert!(parser.push(rec(&send_option("id=3"), 0)).is_empty());

        let rejected = parser.push(rec(
            &send_option(
                "selectedOption=bad selectedSubOption=-1 selectedTarget=0 selectedPosition=0",
            ),
            1,
        ));
        assert_eq!(rejected.len(), 2);
        assert!(rejected
            .iter()
            .all(|packet| matches!(packet, Packet::Unknown(_))));
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn incomplete_send_option_finishes_as_unknown_lines() {
        let mut parser = PowerLogParser::new();
        assert!(parser.push(rec(&send_option("id=3"), 0)).is_empty());
        assert!(parser
            .push(rec(&send_option("selectedOption=2"), 1))
            .is_empty());

        let finished = parser.finish();
        assert_eq!(finished.len(), 2);
        assert!(finished
            .iter()
            .all(|packet| matches!(packet, Packet::Unknown(_))));
    }

    #[test]
    fn send_option_lookalike_key_cannot_satisfy_completeness() {
        let mut parser = PowerLogParser::new();
        assert!(parser
            .push(rec(
                &send_option(
                    "xselectedOption=bad selectedSubOption=-1 selectedTarget=0 selectedPosition=0",
                ),
                0,
            ))
            .is_empty());

        let finished = parser.finish();
        assert_eq!(finished.len(), 1);
        assert!(matches!(finished[0], Packet::Unknown(_)));
    }

    #[test]
    fn parses_entity_choices() {
        let lines = [
            choices("id=1 Player=2 TaskList=4 ChoiceType=MULLIGAN CountMin=0 CountMax=3"),
            choices("  Source=GameEntity"),
            choices(
                "  Entities[0]=[entityName=UNKNOWN ENTITY [cardType=INVALID] id=12 zone=HAND zonePos=1 cardId= player=1]",
            ),
            choices(
                "  Entities[1]=[entityName=UNKNOWN ENTITY [cardType=INVALID] id=13 zone=HAND zonePos=2 cardId= player=1]",
            ),
        ];
        let packets = parse_lines(&lines);
        let Packet::Choices(ch) = &packets[0] else {
            panic!("expected Choices");
        };
        assert_eq!(ch.id, 1);
        assert_eq!(ch.task_list, Some(4));
        assert_eq!(ch.player_entity, Some(EntityId(2)));
        assert_eq!(ch.player_raw, "2");
        assert_eq!(ch.choice_type, "MULLIGAN");
        assert_eq!(ch.min, 0);
        assert_eq!(ch.max, 3);
        assert_eq!(ch.source, Some(EntityId(1)));
        assert_eq!(ch.source_raw, "GameEntity");
        assert_eq!(ch.entities, vec![EntityId(12), EntityId(13)]);
    }

    #[test]
    fn choices_player_name_stays_raw() {
        let line = choices(
            "id=1 Player=UNKNOWN HUMAN PLAYER TaskList=4 ChoiceType=MULLIGAN CountMin=0 CountMax=3",
        );
        let packets = parse_lines(&[line]);
        let Packet::Choices(ch) = &packets[0] else {
            panic!("expected Choices");
        };
        assert_eq!(ch.player_entity, None);
        assert_eq!(ch.player_raw, "UNKNOWN HUMAN PLAYER");
    }

    #[test]
    fn parses_entities_chosen() {
        let lines = [
            chosen("id=1 Player=2 TaskList=8 Source=GameEntity EntitiesCount=1"),
            chosen(
                "  Entities[0]=[entityName=UNKNOWN ENTITY [cardType=INVALID] id=12 zone=HAND cardId= player=1]",
            ),
        ];
        let packets = parse_lines(&lines);
        let Packet::ChosenEntities(ch) = &packets[0] else {
            panic!("expected ChosenEntities");
        };
        assert_eq!(ch.id, 1);
        assert_eq!(ch.task_list, Some(8));
        assert_eq!(ch.player_entity, Some(EntityId(2)));
        assert_eq!(ch.player_raw, "2");
        assert_eq!(ch.source, Some(EntityId(1)));
        assert_eq!(ch.source_raw, "GameEntity");
        assert_eq!(ch.entities, vec![EntityId(12)]);
    }

    #[test]
    fn parses_send_choices() {
        let lines = [
            send_choices(
                "id=1 Player=2 TaskList=8 ChoiceType=MULLIGAN Source=GameEntity",
            ),
            send_choices(
                "  m_chosenEntities[0]=[entityName=UNKNOWN ENTITY [cardType=INVALID] id=12 zone=HAND cardId= player=1]",
            ),
        ];
        let packets = parse_lines(&lines);
        let Packet::SendChoices(send) = &packets[0] else {
            panic!("expected SendChoices");
        };
        assert_eq!(send.id, 1);
        assert_eq!(send.task_list, Some(8));
        assert_eq!(send.player_entity, Some(EntityId(2)));
        assert_eq!(send.player_raw, "2");
        assert_eq!(send.choice_type, "MULLIGAN");
        assert_eq!(send.source, Some(EntityId(1)));
        assert_eq!(send.source_raw, "GameEntity");
        assert_eq!(send.entities, vec![EntityId(12)]);
    }

    #[test]
    fn power_task_list_is_unknown() {
        let line = "D 21:35:47.0000000 PowerTaskList.DebugPrintPower() - BLOCK_START BlockType=TRIGGER Entity=GameEntity EffectCardId=System";
        let packets = parse_lines(&[line.to_string()]);
        let Packet::Unknown(unknown) = &packets[0] else {
            panic!("expected Unknown for PowerTaskList");
        };
        assert!(unknown.record.raw_line.contains("PowerTaskList"));
        assert_eq!(unknown.record.parse_status, ParseStatus::Unrecognized);
    }

    #[test]
    fn unrecognized_power_opcode_is_unknown() {
        let line = power("META_DATA - Meta=TARGET Data=0 InfoCount=1");
        let packets = parse_lines(&[line]);
        assert!(matches!(packets[0], Packet::Unknown(_)));
    }
}
