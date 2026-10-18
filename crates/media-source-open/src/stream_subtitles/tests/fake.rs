//! Подставной транспорт и помощники ожидания для тестов ленты.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use source_core::CancellationToken;
use subtitle_core::SubtitleEvent;
use subtitle_formats::{
    LegacyEncodingHint, SubtitleParseLimits, TextSubtitleFormat, load_text_subtitle,
};

use crate::stream_subtitles::feed::OutagePolicy;
use crate::stream_subtitles::{
    DecodedSegment, PendingSegmentDecode, PlayerTimeSpan, SegmentDecode, StreamIndexRefresh,
    StreamPlaybackPosition, StreamSegment, StreamSegmentIndex, StreamSegmentKey,
    StreamSegmentLimits, StreamSegmentRef, StreamSubtitleError, StreamSubtitleFeed,
    StreamSubtitleFeedConfig, StreamSubtitleMessage, StreamSubtitleSource, StreamSubtitleTrackId,
    StreamSubtitleTrackInfo, StreamSubtitleTransport, StreamTimelineEpoch,
};

pub(super) const WAIT: Duration = Duration::from_secs(5);

/// Реплика с заданными временем (мс) и текстом: настоящий разбор WebVTT.
pub(super) fn cue(start_ms: u32, end_ms: u32, text: &str) -> SubtitleEvent {
    let vtt = format!(
        "WEBVTT\n\n{} --> {}\n{text}\n",
        timestamp(start_ms),
        timestamp(end_ms)
    );
    let loaded = load_text_subtitle(
        vtt.as_bytes(),
        TextSubtitleFormat::WebVtt,
        LegacyEncodingHint::Generic,
        &SubtitleParseLimits::new(4_096, 16, 4),
    )
    .expect("test cue parses");
    loaded.document().events()[0].clone()
}

fn timestamp(total_ms: u32) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        total_ms / 3_600_000,
        total_ms / 60_000 % 60,
        total_ms / 1_000 % 60,
        total_ms % 1_000
    )
}

/// Готовый результат сегмента: одна реплика с номером сегмента в тексте.
pub(super) fn ready_segment(key: u64, segment_start: Option<u32>) -> SegmentDecode {
    let start_ms = u32::try_from(key).expect("small key") * 10_000;
    SegmentDecode::Ready(DecodedSegment {
        events: vec![cue(start_ms, start_ms + 1_000, &format!("segment {key}"))],
        covered: Some(PlayerTimeSpan {
            start: subtitle_core::SubtitleTime::from_millis(i64::from(start_ms)),
            end: subtitle_core::SubtitleTime::from_millis(i64::from(start_ms) + 10_000),
        }),
        segment_start: segment_start
            .map(|ms| subtitle_core::SubtitleTime::from_millis(i64::from(ms))),
    })
}

/// Индекс из `count` сегментов по 10 с, ключи начинаются с `first_key`.
pub(super) fn index(first_key: u64, count: u64, refresh: StreamIndexRefresh) -> StreamSegmentIndex {
    StreamSegmentIndex {
        segments: (first_key..first_key + count)
            .map(|key| StreamSegment {
                key: StreamSegmentKey::new(key),
                reference: StreamSegmentRef::new(key),
                start: Duration::from_secs(key * 10),
                duration: Duration::from_secs(10),
            })
            .collect(),
        refresh,
    }
}

type IndexScript = dyn Fn(usize) -> Result<StreamSegmentIndex, StreamSubtitleError> + Send + Sync;
type SegmentScript =
    dyn Fn(StreamSegmentKey) -> Result<SegmentDecode, StreamSubtitleError> + Send + Sync;

/// Транспорт, поведение которого задают тесты; запоминает порядок скачивания.
pub(super) struct FakeTransport {
    index_script: Box<IndexScript>,
    segment_script: Box<SegmentScript>,
    index_calls: AtomicUsize,
    fetched: Mutex<Vec<u64>>,
    /// Поколение шкалы `start`; тесты меняют его так же, как настоящий транспорт при разрыве.
    timeline_epoch: AtomicU64,
}

impl FakeTransport {
    pub(super) fn new(
        index_script: impl Fn(usize) -> Result<StreamSegmentIndex, StreamSubtitleError>
        + Send
        + Sync
        + 'static,
        segment_script: impl Fn(StreamSegmentKey) -> Result<SegmentDecode, StreamSubtitleError>
        + Send
        + Sync
        + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            index_script: Box::new(index_script),
            segment_script: Box::new(segment_script),
            index_calls: AtomicUsize::new(0),
            fetched: Mutex::new(Vec::new()),
            timeline_epoch: AtomicU64::new(0),
        })
    }

    /// Объявляет, что шкала `start` закреплена заново (вступает в силу со следующего индекса).
    pub(super) fn set_timeline_epoch(&self, epoch: u64) {
        self.timeline_epoch.store(epoch, Ordering::SeqCst);
    }

    /// Сколько раз лента загружала индекс.
    pub(super) fn index_loads(&self) -> usize {
        self.index_calls.load(Ordering::SeqCst)
    }

    /// Очищает журнал скачанных сегментов (дальше смотрим только новые запросы).
    pub(super) fn clear_fetch_log(&self) {
        self.fetched.lock().expect("fetched log").clear();
    }

    pub(super) fn fetched(&self) -> Vec<u64> {
        self.fetched.lock().expect("fetched log").clone()
    }
}

impl StreamSubtitleTransport for FakeTransport {
    fn tracks(&self) -> Vec<StreamSubtitleTrackInfo> {
        vec![StreamSubtitleTrackInfo {
            id: StreamSubtitleTrackId::new(0),
            language: Some("en".into()),
            name: "English".into(),
            forced: false,
            default: true,
            autoselect: true,
        }]
    }

    fn load_index(
        &self,
        _track: StreamSubtitleTrackId,
        _cancellation: &CancellationToken,
    ) -> Result<StreamSegmentIndex, StreamSubtitleError> {
        let call = self.index_calls.fetch_add(1, Ordering::SeqCst);
        (self.index_script)(call)
    }

    fn index_timeline_epoch(&self, _track: StreamSubtitleTrackId) -> StreamTimelineEpoch {
        StreamTimelineEpoch::new(self.timeline_epoch.load(Ordering::SeqCst))
    }

    fn fetch_and_decode(
        &self,
        segment: &StreamSegment,
        _limits: StreamSegmentLimits,
        _cancellation: &CancellationToken,
    ) -> Result<SegmentDecode, StreamSubtitleError> {
        self.fetched
            .lock()
            .expect("fetched log")
            .push(segment.key.value());
        (self.segment_script)(segment.key)
    }
}

/// Сегмент, который досчитывается, когда тест поднимет флаг.
pub(super) struct GatedPending {
    pub(super) released: Arc<std::sync::atomic::AtomicBool>,
    pub(super) key: u64,
}

impl PendingSegmentDecode for GatedPending {
    fn try_resolve(self: Box<Self>) -> Result<SegmentDecode, StreamSubtitleError> {
        if self.released.load(Ordering::SeqCst) {
            Ok(ready_segment(self.key, None))
        } else {
            Ok(SegmentDecode::Pending(self))
        }
    }
}

/// Сегмент, который после отпускания защёлки привязывается с точным началом на шкале плеера
/// (`playlist_start + PLAYER_OFFSET_SECS`): так настоящий транспорт сообщает привязку live.
pub(super) struct AnchoredPending {
    pub(super) released: Arc<std::sync::atomic::AtomicBool>,
    pub(super) key: u64,
}

/// Смещение «плеер = плейлист + 1000 с», которое сообщают привязанные сегменты.
pub(super) const PLAYER_OFFSET_SECS: u64 = 1_000;

impl PendingSegmentDecode for AnchoredPending {
    fn try_resolve(self: Box<Self>) -> Result<SegmentDecode, StreamSubtitleError> {
        if self.released.load(Ordering::SeqCst) {
            let anchor_ms = u32::try_from((self.key * 10 + PLAYER_OFFSET_SECS) * 1_000)
                .expect("small test anchor");
            Ok(ready_segment(self.key, Some(anchor_ms)))
        } else {
            Ok(SegmentDecode::Pending(self))
        }
    }
}

/// Короткие сроки повторов: пауза 20-60 мс, бюджет перебоя `budget`.
pub(super) fn quick_outage_policy(budget: Duration) -> OutagePolicy {
    OutagePolicy {
        initial_backoff: Duration::from_millis(20),
        maximum_backoff: Duration::from_millis(60),
        budget,
    }
}

/// Конфигурация с явными тестовыми значениями (в production значений по умолчанию нет).
pub(super) fn config() -> StreamSubtitleFeedConfig {
    StreamSubtitleFeedConfig {
        ahead: Duration::from_secs(20),
        behind: Duration::from_secs(10),
        max_segment_bytes: std::num::NonZeroUsize::new(1_024).expect("non-zero"),
        max_pending_segments: std::num::NonZeroUsize::new(4).expect("non-zero"),
        failure_threshold: std::num::NonZeroU32::new(3).expect("non-zero"),
        outage_budget: Duration::from_secs(60),
        message_capacity: std::num::NonZeroUsize::new(64).expect("non-zero"),
    }
}

/// Лента + счётчик вызовов колбэка пробуждения.
pub(super) struct Harness {
    pub(super) feed: StreamSubtitleFeed,
    pub(super) position: StreamPlaybackPosition,
    pub(super) wakes: Arc<AtomicUsize>,
}

pub(super) fn start(
    transport: Arc<FakeTransport>,
    config: StreamSubtitleFeedConfig,
    position_secs: u64,
) -> Harness {
    start_with_outage_policy(
        transport,
        config,
        position_secs,
        OutagePolicy::with_budget(config.outage_budget),
    )
}

/// Как [`start`], но со своими сроками повторов при сетевом перебое (короткими для тестов).
pub(super) fn start_with_outage_policy(
    transport: Arc<FakeTransport>,
    config: StreamSubtitleFeedConfig,
    position_secs: u64,
    outage_policy: OutagePolicy,
) -> Harness {
    let source = StreamSubtitleSource::new(transport).expect("fake source has a track");
    let position = StreamPlaybackPosition::new(Duration::from_secs(position_secs));
    let wakes = Arc::new(AtomicUsize::new(0));
    let wake_counter = Arc::clone(&wakes);
    let feed = StreamSubtitleFeed::start_with_outage_policy(
        &source,
        StreamSubtitleTrackId::new(0),
        config,
        position.clone(),
        Arc::new(move || {
            wake_counter.fetch_add(1, Ordering::SeqCst);
        }),
        outage_policy,
    )
    .expect("feed starts");
    Harness {
        feed,
        position,
        wakes,
    }
}

impl Harness {
    /// Копит сообщения, пока `done` не станет истинным; паника по таймауту.
    pub(super) fn collect_until(
        &self,
        done: impl Fn(&[StreamSubtitleMessage]) -> bool,
    ) -> Vec<StreamSubtitleMessage> {
        let deadline = Instant::now() + WAIT;
        let mut messages = Vec::new();
        while !done(&messages) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for feed messages, got {messages:?}"
            );
            messages.extend(self.feed.drain_messages());
            std::thread::sleep(Duration::from_millis(5));
        }
        messages
    }

    /// Сообщения, пришедшие за ближайшие `quiet` (для проверки «больше ничего не будет»).
    pub(super) fn messages_within(&self, quiet: Duration) -> Vec<StreamSubtitleMessage> {
        std::thread::sleep(quiet);
        self.feed.drain_messages()
    }
}

/// Ключи сегментов из сообщений `Events` в порядке прихода.
pub(super) fn event_segments(messages: &[StreamSubtitleMessage]) -> Vec<u64> {
    messages
        .iter()
        .filter_map(|message| match message {
            StreamSubtitleMessage::Events(events) => Some(events.segment.value()),
            _ => None,
        })
        .collect()
}
