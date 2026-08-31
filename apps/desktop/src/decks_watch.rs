//! Decks.log 自动导入(M2 PRD 决策 5)。
//!
//! 与 Power.log 同一套 tail 机制:定时 poll → `DecksLogParser` → 落库。
//! 没见过的牌组自动新建并激活,见过的直接激活;失败只记状态,不影响对局追踪。

use std::time::Duration;

use chrono::NaiveTime;
use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use hs_log_core::decks_log::{DeckObserveKind, DecksLogParser, DecksLogRecord, ObservedDeck};
use hs_log_tailer::{FileTailer, LogTail, TailError};
use hs_types::DeckId;

use crate::stores::Stores;

/// 全局句柄,便于 Settings 页展示状态。
pub struct DecksWatchHandle(pub Entity<DecksWatcher>);

impl Global for DecksWatchHandle {}

pub struct DecksWatcher {
    tailer: Option<FileTailer>,
    parser: DecksLogParser,
    /// 最近一次自动导入结果(UI 展示)。
    pub status: String,
    /// 已自动导入的牌组数(本次会话)。
    pub imported: usize,
    /// 轮询计数,用于节流重新定位。
    ticks: u32,
    /// 仅 `Finding Game With Deck` 的时间线,供对局按开局时钟认牌组。
    activations: Vec<(NaiveTime, DeckId)>,
    _task: Option<Task<()>>,
}

/// 重新定位的轮询间隔(秒):炉石可能在应用之后才启动,
/// 新会话目录出现后要能自动切换过去。
const RELOCATE_EVERY_TICKS: u32 = 5;

impl DecksWatcher {
    pub fn new() -> Self {
        Self {
            tailer: None,
            parser: DecksLogParser::new(),
            status: "未开始(尚未定位 Decks.log)".into(),
            imported: 0,
            ticks: 0,
            activations: Vec::new(),
            _task: None,
        }
    }

    /// 开局时钟之前最近一次排队选用的牌组。
    pub fn deck_at(&self, clock: NaiveTime) -> Option<DeckId> {
        self.activations
            .iter()
            .filter(|(t, _)| *t <= clock)
            .max_by_key(|(t, _)| *t)
            .map(|(_, id)| *id)
    }

    /// 定位当前活跃会话目录下的 Decks.log。目录变化(炉石重启换了
    /// 会话目录)时切换 tail 并重置解析器;定不到位只更新状态,下轮再试。
    fn relocate(&mut self, cx: &mut Context<Self>) {
        use hs_platform::HearthstonePlatform;
        let platform = hs_platform::macos::MacPlatform;
        let candidates = platform.locate_log_candidates().unwrap_or_default();
        let Some(candidate) = hs_platform::pick_active_candidate(&candidates) else {
            if self.tailer.is_none() {
                self.status = "未找到炉石日志目录".into();
                cx.notify();
            }
            return;
        };
        let path = candidate.dir.join("Decks.log");
        if self.tailer.as_ref().is_some_and(|t| t.path() == path) {
            return;
        }
        self.status = format!("监视 {}", path.display());
        self.tailer = Some(FileTailer::new(path.to_string_lossy().into_owned()));
        self.parser = DecksLogParser::new();
        self.activations.clear();
        cx.notify();
    }

    /// 定位炉石日志目录下的 Decks.log 并常驻 tail。
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.relocate(cx);

        let weak = cx.entity().downgrade();
        self._task = Some(cx.spawn(async move |_, cx| loop {
            let timer =
                match cx.update(|cx| cx.background_executor().timer(Duration::from_millis(1000))) {
                    Ok(t) => t,
                    Err(_) => break,
                };
            timer.await;
            // tailer 可能暂时定位不到(炉石还没启动),循环要保持运转,
            // 只有实体销毁才退出。
            let alive = weak.update(cx, |this, cx| this.poll(cx)).is_ok();
            if !alive {
                break;
            }
        }));
        self.poll(cx);
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        self.ticks = self.ticks.wrapping_add(1);
        if self.tailer.is_none() || self.ticks.is_multiple_of(RELOCATE_EVERY_TICKS) {
            self.relocate(cx);
        }
        let Some(mut tailer) = self.tailer.take() else {
            return;
        };
        match tailer.poll() {
            Ok(records) if !records.is_empty() => {
                let mut observed = Vec::new();
                for record in records {
                    for parsed in self.parser.push(&record.raw_line) {
                        if let DecksLogRecord::Deck(deck) = parsed {
                            observed.push(deck);
                        }
                    }
                }
                if !observed.is_empty() {
                    self.ingest(observed, cx);
                }
            }
            Ok(_) => {}
            Err(TailError::Truncated { .. }) => {
                // 新会话重建了文件:解析器重置,下轮从头读
                self.parser = DecksLogParser::new();
                self.activations.clear();
            }
            Err(_) => {} // 文件尚未出现:静默等待
        }
        self.tailer = Some(tailer);
    }

    /// 收藏段只入库不抢激活;排队段才激活并记入时间线。
    fn ingest(&mut self, decks: Vec<ObservedDeck>, cx: &mut Context<Self>) {
        let Some(repo) = cx.global::<Stores>().decks.clone() else {
            self.status = "存储未就绪,自动导入已跳过".into();
            cx.notify();
            return;
        };
        for deck in decks {
            let Some(deckstring) = deck.deckstring.as_deref() else {
                continue; // 段里没有牌组代码：无法识别，留给手动导入
            };
            let name = deck.name.as_deref().unwrap_or("自动导入的牌组");
            let queued = deck.kind == DeckObserveKind::FindingGame;
            let result = if queued {
                repo.ingest_observed_deck(name, deckstring)
            } else {
                repo.upsert_observed_deck(name, deckstring)
            };
            match result {
                Ok(id) => {
                    self.imported += 1;
                    if queued {
                        if let Some(clock) =
                            deck.observed_at.as_deref().and_then(parse_observe_clock)
                        {
                            self.activations.push((clock, id));
                        }
                        self.status = format!("已自动激活「{name}」");
                    }
                }
                Err(err) => self.status = format!("自动导入失败: {err}"),
            }
        }
        cx.notify();
    }
}

fn parse_observe_clock(token: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(token, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(token, "%H:%M:%S"))
        .ok()
}

/// 创建监视器、注册全局句柄并开始监视。
pub fn init(cx: &mut App) -> Entity<DecksWatcher> {
    let watcher = cx.new(|cx| {
        let mut w = DecksWatcher::new();
        w.start(cx);
        w
    });
    cx.set_global(DecksWatchHandle(watcher.clone()));
    watcher
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deck_at_uses_latest_queue_not_a_later_one() {
        let mut watcher = DecksWatcher::new();
        watcher
            .activations
            .push((NaiveTime::from_hms_opt(22, 42, 16).unwrap(), DeckId(9)));
        watcher
            .activations
            .push((NaiveTime::from_hms_opt(23, 18, 1).unwrap(), DeckId(11)));
        assert_eq!(
            watcher.deck_at(NaiveTime::from_hms_opt(22, 42, 22).unwrap()),
            Some(DeckId(9))
        );
        assert_eq!(
            watcher.deck_at(NaiveTime::from_hms_opt(23, 18, 8).unwrap()),
            Some(DeckId(11))
        );
        assert_eq!(
            watcher.deck_at(NaiveTime::from_hms_opt(22, 0, 0).unwrap()),
            None
        );
    }
}
