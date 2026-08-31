//! 对局会话(M2 Track D):tail → parse → normalize → reduce 管道的唯一所有者。
//!
//! Inspector / Live 页 / Overlay 都观察本实体读同一份状态,
//! 不允许任何 UI 复制第二套游戏状态(设计文档 §6.1/§23-9)。

use std::time::Duration;

use chrono::{Local, NaiveDate, NaiveTime, TimeZone};
use gpui::{App, AppContext as _, Context, Entity, Global, SharedString, Task, WeakEntity};
use hs_decks::GameRecord;
use hs_log_core::{DomainEvent, EventNormalizer, Packet, PowerEventNormalizer, PowerLogParser};
use hs_log_tailer::{FileTailer, LogTail, RawLogRecord, TailError};
use hs_observed_state::{reduce, ObservedGameState};
use hs_types::{EntityId, GameId, GamePhase, PlayerId};
use std::collections::BTreeSet;
use std::path::Path;

use crate::stores::Stores;

pub const FIXTURE_LOG: &str = "tests/logs/999999-game-open.powerlog.txt";

struct Pipeline {
    tailer: FileTailer,
    parser: PowerLogParser,
    normalizer: PowerEventNormalizer,
    decision_quiet: DecisionQuiet,
}

enum PipelineItem {
    Raw(RawLogRecord),
    Packet(Packet),
}

/// 决策包的静默提交门闩：有新行后，首个空 poll 触发一次提交。
///
/// 它只负责调度时机；Parser 自己决定当前 pending 是否属于 Options/Choices。
#[derive(Debug, Default)]
struct DecisionQuiet {
    saw_input: bool,
}

impl DecisionQuiet {
    fn observe_poll(&mut self, has_records: bool) -> bool {
        if has_records {
            self.saw_input = true;
            false
        } else {
            std::mem::take(&mut self.saw_input)
        }
    }
}

impl Pipeline {
    /// Parse one tailer poll. This is the production quiet-commit boundary and
    /// intentionally leaves non-decision packets pending on an empty poll.
    fn parse_poll(&mut self, records: Vec<RawLogRecord>) -> Vec<PipelineItem> {
        if records.is_empty() {
            return if self.decision_quiet.observe_poll(false) {
                self.parser
                    .flush_pending_decision()
                    .into_iter()
                    .map(PipelineItem::Packet)
                    .collect()
            } else {
                Vec::new()
            };
        }

        self.decision_quiet.observe_poll(true);
        let mut items = Vec::new();
        for record in records {
            let packets = self.parser.push(record.clone());
            let mut raw = Some(record);
            for packet in packets {
                if raw
                    .as_ref()
                    .is_some_and(|record| packet_contains_record(&packet, record))
                {
                    items.push(PipelineItem::Raw(raw.take().expect("raw record exists")));
                }
                items.push(PipelineItem::Packet(packet));
            }
            if let Some(record) = raw {
                items.push(PipelineItem::Raw(record));
            }
        }
        items
    }
}

fn packet_contains_record(packet: &Packet, record: &RawLogRecord) -> bool {
    packet.source_records().iter().any(|source| {
        source.session_id == record.session_id
            && source.file_id == record.file_id
            && source.byte_offset == record.byte_offset
    })
}

pub struct RawRow {
    pub offset: u64,
    pub text: SharedString,
}

pub struct PacketRow {
    pub label: SharedString,
    /// 源 Raw 行的 byte_offset 集合(选中联动高亮)。
    pub record_offsets: Vec<u64>,
}

pub struct EventRow {
    pub label: SharedString,
    pub packet_idx: usize,
    pub entity: Option<EntityId>,
}

/// 全局会话句柄:窗口/视图构造时从这里取。
pub struct SessionHandle(pub Entity<GameSession>);

impl Global for SessionHandle {}

pub struct GameSession {
    pub source: SharedString,
    pipeline: Option<Pipeline>,
    pub raw_rows: Vec<RawRow>,
    pub packet_rows: Vec<PacketRow>,
    pub event_rows: Vec<EventRow>,
    /// 全量事件序列(Inspector 步进重算的数据源)。
    pub events: Vec<DomainEvent>,
    /// 实时归约状态(增量维护)。
    pub state: ObservedGameState,
    /// 本局原始日志行(用于对局结束时存档,PRD 决策 4)。
    game_log_lines: Vec<String>,
    /// 已落库的对局,避免重复写入。
    recorded_games: BTreeSet<GameId>,
    /// 最近一次落库结果(Settings/History 页可见)。
    pub last_record_status: Option<String>,
    /// 跟随当前活跃会话目录(启动/「检测真实日志」为真;
    /// Inspector 打开 fixture 为假,避免被真机日志抢走)。
    follow_active: bool,
    /// 轮询计数,用于节流重新定位。
    ticks: u32,
    _pump_task: Option<Task<()>>,
}

/// 重新定位间隔(tick × 400ms):炉石后启动或换会话目录后要能跟过去。
const RELOCATE_EVERY_TICKS: u32 = 5;

impl GameSession {
    pub fn new() -> Self {
        Self {
            source: "未选择日志".into(),
            pipeline: None,
            raw_rows: Vec::new(),
            packet_rows: Vec::new(),
            event_rows: Vec::new(),
            events: Vec::new(),
            state: ObservedGameState::default(),
            game_log_lines: Vec::new(),
            recorded_games: BTreeSet::new(),
            last_record_status: None,
            follow_active: false,
            ticks: 0,
            _pump_task: None,
        }
    }

    /// 从头归约前 `cursor` 个事件(Inspector 步进用;一局事件量毫秒级)。
    pub fn replay_to(&self, cursor: usize) -> ObservedGameState {
        let mut state = ObservedGameState::default();
        for event in &self.events[..cursor.min(self.events.len())] {
            if let Ok(next) = reduce(&state, event) {
                state = next;
            }
        }
        state
    }

    /// 绑到指定 Power.log(不改 follow_active)。路径没变则不重置管道。
    fn attach_path(&mut self, path: String, cx: &mut Context<Self>) {
        if self
            .pipeline
            .as_ref()
            .is_some_and(|p| p.tailer.path() == Path::new(&path))
        {
            return;
        }
        self.source = SharedString::from(path.clone());
        self.pipeline = Some(Pipeline {
            tailer: FileTailer::new(path),
            parser: PowerLogParser::new(),
            normalizer: PowerEventNormalizer::new(),
            decision_quiet: DecisionQuiet::default(),
        });
        self.reset_rows();
        cx.notify();
    }

    fn ensure_pump(&mut self, cx: &mut Context<Self>) {
        if self._pump_task.is_some() {
            return;
        }
        let weak: WeakEntity<Self> = cx.entity().downgrade();
        self._pump_task = Some(cx.spawn(async move |_, cx| loop {
            let timer =
                match cx.update(|cx| cx.background_executor().timer(Duration::from_millis(400))) {
                    Ok(t) => t,
                    Err(_) => break,
                };
            timer.await;
            // 实体销毁才退出;pipeline 暂时空(炉石还没起来)也要继续转。
            if weak.update(cx, |this, cx| this.pump(cx)).is_err() {
                break;
            }
        }));
    }

    /// 定位当前活跃会话目录下的 Power.log。目录变了就切管道并重置状态。
    fn relocate(&mut self, cx: &mut Context<Self>) {
        use hs_platform::HearthstonePlatform;
        let platform = hs_platform::macos::MacPlatform;
        let candidates = platform.locate_log_candidates().unwrap_or_default();
        let Some(candidate) = hs_platform::pick_active_candidate(&candidates) else {
            if self.pipeline.is_none() {
                self.source = "未找到炉石日志目录（可先用 fixture）".into();
                cx.notify();
            }
            return;
        };
        self.attach_path(
            candidate
                .dir
                .join("Power.log")
                .to_string_lossy()
                .into_owned(),
            cx,
        );
    }

    pub fn open_path(&mut self, path: String, cx: &mut Context<Self>) {
        self.follow_active = false;
        self.attach_path(path, cx);
        self.ensure_pump(cx);
        self.pump(cx);
    }

    pub fn detect_real_log(&mut self, cx: &mut Context<Self>) {
        self.follow_active = true;
        self.relocate(cx);
        self.ensure_pump(cx);
        self.pump(cx);
    }

    fn reset_rows(&mut self) {
        self.raw_rows.clear();
        self.packet_rows.clear();
        self.event_rows.clear();
        self.events.clear();
        self.state = ObservedGameState::default();
        self.game_log_lines.clear();
        // 换文件后 GameId 从 1 重计,必须忘掉上一份日志的已落库集合。
        self.recorded_games.clear();
    }

    /// 对局结束落库:结果入 `game_sessions`/`game_players`/`game_results`,
    /// 原始日志段存档到数据目录(PRD 决策 4)。存档失败只降级,不影响统计。
    fn record_finished_game(&mut self, cx: &mut Context<Self>) {
        let Some(game_id) = self.state.game_id else {
            return; // 没有 GameId 无法定位这一局，跳过
        };
        if !self.recorded_games.insert(game_id) {
            return;
        }
        let Some(history) = cx.global::<Stores>().history.clone() else {
            self.last_record_status = Some("存储未就绪,本局未落库".into());
            return;
        };

        // 原始日志存档：失败仅记 warning，路径/哈希留空。
        let data_dir = hs_platform::app_data_dir();
        let log_text = self.game_log_lines.join("\n");
        let archive = match hs_decks::archive_power_log(&data_dir, game_id, &log_text) {
            Ok(archive) => Some(archive),
            Err(err) => {
                tracing::warn!("archive power log for game {} failed: {err}", game_id.0);
                None
            }
        };

        let (started_at, ended_at) = game_time_range(&self.source, &self.game_log_lines);
        let (friendly_class, opposing_class) = infer_classes(&self.state, cx);
        let start_clock = self.game_log_lines.iter().find_map(|l| parse_log_clock(l));
        let mut record = GameRecord::new(game_id);
        record.deck_id = deck_id_for_game(cx, start_clock);
        record.friendly_class = friendly_class;
        record.opposing_class = opposing_class;
        record.friendly_player = self.state.friendly_player;
        record.winner = self.state.winner;
        record.started_at = started_at;
        record.ended_at = ended_at;
        record.raw_log_path = archive
            .as_ref()
            .map(|a| a.path.to_string_lossy().into_owned());
        record.raw_log_sha256 = archive.as_ref().map(|a| a.sha256.clone());
        self.last_record_status = Some(match history.record_game(&record) {
            Ok(()) => {
                let result = match (self.state.winner, self.state.friendly_player) {
                    (Some(w), Some(f)) if w == f => "胜",
                    (Some(_), Some(_)) => "负",
                    _ => "结果未知",
                };
                format!("对局 {} 已落库({result})", game_id.0)
            }
            Err(err) => format!("落库失败: {err}"),
        });
    }

    /// 单次管道推进:tail → parse → normalize → reduce。
    fn pump(&mut self, cx: &mut Context<Self>) {
        self.ticks = self.ticks.wrapping_add(1);
        if self.follow_active
            && (self.pipeline.is_none() || self.ticks.is_multiple_of(RELOCATE_EVERY_TICKS))
        {
            self.relocate(cx);
        }
        let Some(mut pipeline) = self.pipeline.take() else {
            return;
        };
        match pipeline.tailer.poll() {
            Ok(records) => {
                let items = pipeline.parse_poll(records);
                let changed = !items.is_empty();
                for item in items {
                    match item {
                        PipelineItem::Raw(record) => {
                            self.raw_rows.push(RawRow {
                                offset: record.byte_offset,
                                text: SharedString::from(truncate(&record.raw_line, 72)),
                            });
                            self.game_log_lines.push(record.raw_line.to_string());
                        }
                        PipelineItem::Packet(packet) => {
                            self.ingest_packet(&mut pipeline.normalizer, &packet, cx);
                        }
                    }
                }
                if changed {
                    cx.notify();
                }
            }
            Err(TailError::Truncated { .. }) => {
                // 文件被重建：整条链路重置，下轮从头读
                pipeline.parser = PowerLogParser::new();
                pipeline.normalizer = PowerEventNormalizer::new();
                pipeline.decision_quiet = DecisionQuiet::default();
                self.reset_rows();
                cx.notify();
            }
            Err(_) => {} // 文件尚未出现等：静默等待下一轮
        }
        self.pipeline = Some(pipeline);
    }

    fn ingest_packet(
        &mut self,
        normalizer: &mut PowerEventNormalizer,
        packet: &Packet,
        cx: &mut Context<Self>,
    ) {
        let packet_idx = self.packet_rows.len();
        self.packet_rows.push(PacketRow {
            label: SharedString::from(packet_label(packet)),
            record_offsets: packet
                .source_records()
                .iter()
                .map(|r| r.byte_offset)
                .collect(),
        });
        let events = normalizer.normalize(packet).unwrap_or_default();
        for event in events {
            self.event_rows.push(EventRow {
                label: SharedString::from(event_label(&event)),
                packet_idx,
                entity: event_entity(&event),
            });
            if matches!(event, DomainEvent::GameCreated { .. }) {
                // 新一局开始:丢掉上一局已落库的行,只留本包 CREATE_GAME 原文。
                self.game_log_lines = packet
                    .source_records()
                    .iter()
                    .map(|r| r.raw_line.to_string())
                    .collect();
            }
            if let Ok(next) = reduce(&self.state, &event) {
                self.state = next;
            }
            // 等胜者写上再落库:PLAYSTATE=LOST 会先把 phase 打成 Ended,WON 还在下一行。
            if self.state.phase == GamePhase::Ended && self.state.winner.is_some() {
                self.record_finished_game(cx);
            }
            self.events.push(event);
        }
    }
}

/// 在 App 级创建会话并注册全局句柄。
pub fn init(cx: &mut App) -> Entity<GameSession> {
    let session = cx.new(|cx| {
        let mut session = GameSession::new();
        session.detect_real_log(cx);
        session
    });
    cx.set_global(SessionHandle(session.clone()));
    session
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

fn packet_label(packet: &Packet) -> String {
    match packet {
        Packet::CreateGame(p) => format!("CREATE_GAME ({} players)", p.players.len()),
        Packet::FullEntity(p) => format!(
            "FULL_ENTITY #{} {}",
            p.entity_id.0,
            p.card_id.as_ref().map(|c| c.0.as_ref()).unwrap_or("?")
        ),
        Packet::TagChange(p) => {
            format!("TAG_CHANGE #{} {}={}", p.entity.0, p.tag_name, p.value_raw)
        }
        Packet::Unknown(p) => format!("UNKNOWN: {}", truncate(&p.record.raw_line, 40)),
        other => format!("{other:?}").chars().take(40).collect(),
    }
}

fn event_label(event: &DomainEvent) -> String {
    match event {
        DomainEvent::GameCreated { game_id, .. } => format!("GameCreated game={}", game_id.0),
        DomainEvent::EntityCreated {
            entity_id, card_id, ..
        } => format!(
            "EntityCreated #{} {}",
            entity_id.0,
            card_id.as_ref().map(|c| c.0.as_ref()).unwrap_or("-")
        ),
        DomainEvent::TagChanged {
            entity_id,
            tag,
            value,
        } => format!("TagChanged #{} tag{}={}", entity_id.0, tag.0, value),
        DomainEvent::ProtocolWarning { message } => {
            format!("Warning: {}", truncate(message, 36))
        }
        other => format!("{other:?}").chars().take(40).collect(),
    }
}

fn event_entity(event: &DomainEvent) -> Option<EntityId> {
    match event {
        DomainEvent::EntityCreated { entity_id, .. }
        | DomainEvent::TagChanged { entity_id, .. }
        | DomainEvent::EntityRevealed { entity_id, .. }
        | DomainEvent::ZoneChanged { entity_id, .. } => Some(*entity_id),
        _ => None,
    }
}

fn deck_id_for_game(cx: &gpui::App, start_clock: Option<NaiveTime>) -> Option<hs_types::DeckId> {
    if let Some(clock) = start_clock {
        if cx.has_global::<crate::decks_watch::DecksWatchHandle>() {
            let watcher = cx
                .global::<crate::decks_watch::DecksWatchHandle>()
                .0
                .clone();
            if let Some(id) = watcher.read(cx).deck_at(clock) {
                return Some(id);
            }
        }
    }
    crate::stores::active_deck_id(cx)
}

fn infer_classes(state: &ObservedGameState, cx: &gpui::App) -> (Option<String>, Option<String>) {
    if !cx.has_global::<crate::cards::CardNames>() {
        return (None, None);
    }
    let names = cx.global::<crate::cards::CardNames>();
    let class_of = |player: Option<PlayerId>| -> Option<String> {
        let pid = player?;
        let hero = state.players.get(&pid.0)?.hero_entity?;
        let card = state.entities.get(&hero)?.card_id.as_ref()?;
        names.class_of_card(card)
    };
    (
        class_of(state.friendly_player),
        class_of(match state.friendly_player {
            Some(PlayerId(1)) => Some(PlayerId(2)),
            Some(PlayerId(2)) => Some(PlayerId(1)),
            _ => None,
        }),
    )
}

/// 会话目录名 `Hearthstone_YYYY_MM_DD_...` 里的日期;没有就用今天。
fn session_date_from_path(path: &Path) -> Option<NaiveDate> {
    let name = path.parent()?.file_name()?.to_str()?;
    let rest = name.strip_prefix("Hearthstone_")?;
    let year: i32 = rest.get(0..4)?.parse().ok()?;
    let month: u32 = rest.get(5..7)?.parse().ok()?;
    let day: u32 = rest.get(8..10)?.parse().ok()?;
    NaiveDate::from_ymd_opt(year, month, day)
}

/// `D HH:MM:SS.frac ...` 行首时钟。
fn parse_log_clock(line: &str) -> Option<NaiveTime> {
    let mut parts = line.splitn(3, ' ');
    let _level = parts.next()?;
    let time_token = parts.next()?;
    NaiveTime::parse_from_str(time_token, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(time_token, "%H:%M:%S"))
        .ok()
}

fn stamp_local(date: NaiveDate, clock: NaiveTime) -> String {
    match Local.from_local_datetime(&date.and_time(clock)) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => dt.to_rfc3339(),
        chrono::LocalResult::None => date.and_time(clock).and_utc().to_rfc3339(),
    }
}

/// 本局第一行/最后一行时钟 + 会话目录日期。跨日则结束日期 +1。
fn game_time_range(source: &str, lines: &[String]) -> (String, Option<String>) {
    let date =
        session_date_from_path(Path::new(source)).unwrap_or_else(|| Local::now().date_naive());
    let start = lines.iter().find_map(|l| parse_log_clock(l));
    let end = lines.iter().rev().find_map(|l| parse_log_clock(l));
    let started = start
        .map(|clock| stamp_local(date, clock))
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let ended = end.map(|clock| {
        let end_date = if start.is_some_and(|s| clock < s) {
            date.succ_opt().unwrap_or(date)
        } else {
            date
        };
        stamp_local(end_date, clock)
    });
    (started, ended)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;
    use hs_log_tailer::ParseStatus;
    use hs_types::{LogFileId, LogSessionId};
    use std::sync::Arc;

    fn log_record(offset: u64, logger: &str, payload: &str) -> RawLogRecord {
        RawLogRecord {
            session_id: LogSessionId(1),
            file_id: LogFileId(1),
            byte_offset: offset,
            timestamp: None,
            raw_line: Arc::from(format!(
                "D 21:35:47.0000000 GameState.{logger}() - {payload}"
            )),
            parse_status: ParseStatus::Pending,
        }
    }

    fn options_record(offset: u64, payload: &str) -> RawLogRecord {
        log_record(offset, "DebugPrintOptions", payload)
    }

    fn power_record(offset: u64, payload: &str) -> RawLogRecord {
        log_record(offset, "DebugPrintPower", payload)
    }

    #[test]
    fn session_date_from_hearthstone_folder() {
        let path =
            Path::new("/Applications/Hearthstone/Logs/Hearthstone_2026_08_22_22_37_10/Power.log");
        assert_eq!(
            session_date_from_path(path),
            NaiveDate::from_ymd_opt(2026, 8, 22)
        );
        assert_eq!(session_date_from_path(Path::new("Power.log")), None);
    }

    #[test]
    fn parse_log_clock_reads_fractional_time() {
        let clock = parse_log_clock("D 22:42:22.9305490 GameState.DebugPrintPower() - CREATE_GAME")
            .expect("clock");
        assert_eq!(clock.hour(), 22);
        assert_eq!(clock.minute(), 42);
        assert_eq!(clock.second(), 22);
    }

    #[test]
    fn game_time_range_uses_session_date_and_wraps_midnight() {
        let source = "/logs/Hearthstone_2026_08_22_22_37_10/Power.log";
        let lines = [
            "D 23:50:00.0000000 start".to_string(),
            "D 00:10:00.0000000 end".to_string(),
        ];
        let (started, ended) = game_time_range(source, &lines);
        assert!(started.contains("2026-08-22T23:50:00"), "{started}");
        let ended = ended.expect("ended");
        assert!(ended.contains("2026-08-23T00:10:00"), "{ended}");
    }

    #[test]
    fn decision_quiet_fires_once_after_input_and_rearms_on_more_input() {
        let mut quiet = DecisionQuiet::default();

        assert!(
            !quiet.observe_poll(false),
            "startup silence has no input to flush"
        );
        assert!(
            !quiet.observe_poll(true),
            "non-empty polls only rearm the gate"
        );
        assert!(
            !quiet.observe_poll(true),
            "additional input stays in the same batch"
        );
        assert!(
            quiet.observe_poll(false),
            "the first quiet poll commits the batch"
        );
        assert!(
            !quiet.observe_poll(false),
            "continued silence must not commit twice"
        );

        assert!(!quiet.observe_poll(true), "new input rearms the gate");
        assert!(
            quiet.observe_poll(false),
            "the next quiet poll commits the new batch"
        );
    }

    #[test]
    fn pipeline_publishes_split_options_only_after_quiet_without_send_option() {
        let mut pipeline = Pipeline {
            tailer: FileTailer::new("unused-test-power.log"),
            parser: PowerLogParser::new(),
            normalizer: PowerEventNormalizer::new(),
            decision_quiet: DecisionQuiet::default(),
        };
        let mut state = ObservedGameState::default();

        let packet_count = |items: &[PipelineItem]| {
            items
                .iter()
                .filter(|item| matches!(item, PipelineItem::Packet(_)))
                .count()
        };
        assert_eq!(
            packet_count(&pipeline.parse_poll(vec![options_record(0, "id=17")])),
            0
        );
        assert_eq!(
            packet_count(&pipeline.parse_poll(vec![options_record(
                1,
                "  option 0 type=POWER mainEntity=42 error=NONE"
            )])),
            0
        );
        assert_eq!(
            packet_count(&pipeline.parse_poll(vec![options_record(
                2,
                "    target 0 entity=7 error=NONE errorParam="
            )])),
            0
        );
        assert!(state.options.is_none(), "non-empty polls must not publish");

        let items = pipeline.parse_poll(Vec::new());
        assert_eq!(packet_count(&items), 1);
        for item in items {
            if let PipelineItem::Packet(packet) = item {
                for event in pipeline.normalizer.normalize(&packet).unwrap() {
                    state = reduce(&state, &event).unwrap();
                }
            }
        }

        let options = state
            .options
            .as_ref()
            .expect("quiet poll publishes Options");
        assert_eq!(options.id, 17);
        assert_eq!(options.options.len(), 1);
        assert_eq!(options.options[0].entity, Some(EntityId(42)));
        assert_eq!(options.options[0].target_entries.len(), 1);
        assert_eq!(
            options.options[0].target_entries[0].entity,
            Some(EntityId(7))
        );
        assert_eq!(packet_count(&pipeline.parse_poll(Vec::new())), 0);
    }

    #[test]
    fn pipeline_orders_triggering_record_after_flushed_create_game() {
        let mut pipeline = Pipeline {
            tailer: FileTailer::new("unused-test-power.log"),
            parser: PowerLogParser::new(),
            normalizer: PowerEventNormalizer::new(),
            decision_quiet: DecisionQuiet::default(),
        };
        let items = pipeline.parse_poll(vec![
            power_record(0, "CREATE_GAME"),
            power_record(1, "FULL_ENTITY - Creating ID=9 CardID=CS2_029"),
        ]);

        assert_eq!(items.len(), 3);
        assert!(matches!(&items[0], PipelineItem::Raw(record) if record.byte_offset == 0));
        assert!(matches!(
            &items[1],
            PipelineItem::Packet(Packet::CreateGame(_))
        ));
        assert!(matches!(&items[2], PipelineItem::Raw(record) if record.byte_offset == 1));

        let mut archived_lines = Vec::new();
        for item in items {
            match item {
                PipelineItem::Raw(record) => archived_lines.push(record.raw_line.to_string()),
                PipelineItem::Packet(Packet::CreateGame(packet)) => {
                    archived_lines = packet
                        .records
                        .iter()
                        .map(|record| record.raw_line.to_string())
                        .collect();
                }
                PipelineItem::Packet(_) => {}
            }
        }
        assert_eq!(archived_lines.len(), 2);
        assert!(archived_lines[0].contains("CREATE_GAME"));
        assert!(archived_lines[1].contains("FULL_ENTITY"));
    }
}
