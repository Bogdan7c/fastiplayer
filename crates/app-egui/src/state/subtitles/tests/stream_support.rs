//! Общие помощники тестов потоковых субтитров: подставной транспорт (настоящий контракт
//! `StreamSubtitleTransport` без сети), владелец с потоком, ожидание ленты и чтение таймлайна.
//!
//! Подставной транспорт играет роль HLS: отдаёт индекс из одинаковых сегментов по `segment_secs`
//! секунд и реплики сегментов, а тест управляет «сетью» — калиткой (сегмент не отдаётся, пока
//! она закрыта), флагом сбоя и счётчиками вызовов.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use media_source_open::stream_subtitles::{
    DecodedSegment, PlayerTimeSpan, SegmentDecode, StreamIndexRefresh, StreamSegment,
    StreamSegmentIndex, StreamSegmentKey, StreamSegmentLimits, StreamSegmentRef,
    StreamSubtitleError, StreamSubtitleSource, StreamSubtitleTrackId, StreamSubtitleTrackInfo,
    StreamSubtitleTransport,
};
use source_core::CancellationToken;
use subtitle_core::{SubtitleEvent, SubtitleTime};
use subtitle_formats::{
    LegacyEncodingHint, SubtitleParseLimits, TextSubtitleFormat, load_text_subtitle,
};

use super::super::owner_stream::StreamSubtitleNotice;
use super::super::stream_position::{StreamPlaybackObservation, StreamTimelineKind};
use super::super::track_list::TrackLoadState;
use super::*;

/// Реплика сегмента: начало и конец в миллисекундах шкалы плеера и текст.
#[derive(Debug, Clone)]
pub(super) struct CueSpec {
    pub(super) start_ms: u32,
    pub(super) end_ms: u32,
    pub(super) text: String,
}

impl CueSpec {
    pub(super) fn new(start_ms: u32, end_ms: u32, text: &str) -> Self {
        Self {
            start_ms,
            end_ms,
            text: text.to_owned(),
        }
    }
}

/// Что лежит в opaque ссылке сегмента этого транспорта.
struct SegmentAddress {
    track: StreamSubtitleTrackId,
    key: u64,
}

/// Подставной транспорт: сегменты и реплики задаёт тест.
pub(super) struct FakeStreamTransport {
    tracks: Vec<StreamSubtitleTrackInfo>,
    segment_secs: u64,
    /// Реплики по ключу сегмента (одинаковые для всех дорожек).
    segments: Vec<Vec<CueSpec>>,
    /// Калитка: пока `false`, `fetch_and_decode` ждёт (или отмену).
    gate_open: AtomicBool,
    /// Сбой сети на каждом сегменте.
    failing: AtomicBool,
    fetch_calls: AtomicUsize,
    index_requests: Mutex<Vec<StreamSubtitleTrackId>>,
    /// Какую-то загрузку отменили (видели флаг отмены).
    cancellation_seen: AtomicBool,
}

impl FakeStreamTransport {
    /// Транспорт с калиткой: `gate_open` определяет, отдаются ли сегменты сразу.
    pub(super) fn new(
        tracks: Vec<StreamSubtitleTrackInfo>,
        segment_secs: u64,
        segments: Vec<Vec<CueSpec>>,
        gate_open: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            tracks,
            segment_secs,
            segments,
            gate_open: AtomicBool::new(gate_open),
            failing: AtomicBool::new(false),
            fetch_calls: AtomicUsize::new(0),
            index_requests: Mutex::new(Vec::new()),
            cancellation_seen: AtomicBool::new(false),
        })
    }

    pub(super) fn open_gate(&self) {
        self.gate_open.store(true, Ordering::Release);
    }

    pub(super) fn set_failing(&self, failing: bool) {
        self.failing.store(failing, Ordering::Release);
    }

    pub(super) fn fetch_calls(&self) -> usize {
        self.fetch_calls.load(Ordering::Acquire)
    }

    pub(super) fn cancellation_seen(&self) -> bool {
        self.cancellation_seen.load(Ordering::Acquire)
    }

    /// Для каких дорожек запрашивали индекс (по порядку).
    pub(super) fn indexed_tracks(&self) -> Vec<StreamSubtitleTrackId> {
        self.index_requests.lock().expect("index log").clone()
    }
}

impl StreamSubtitleTransport for FakeStreamTransport {
    fn tracks(&self) -> Vec<StreamSubtitleTrackInfo> {
        self.tracks.clone()
    }

    fn load_index(
        &self,
        track: StreamSubtitleTrackId,
        _cancellation: &CancellationToken,
    ) -> Result<StreamSegmentIndex, StreamSubtitleError> {
        self.index_requests.lock().expect("index log").push(track);
        let segments = (0..self.segments.len() as u64)
            .map(|key| StreamSegment {
                key: StreamSegmentKey::new(key),
                reference: StreamSegmentRef::new(SegmentAddress { track, key }),
                start: Duration::from_secs(key * self.segment_secs),
                duration: Duration::from_secs(self.segment_secs),
            })
            .collect();
        Ok(StreamSegmentIndex {
            segments,
            refresh: StreamIndexRefresh::Never,
        })
    }

    fn fetch_and_decode(
        &self,
        segment: &StreamSegment,
        _limits: StreamSegmentLimits,
        cancellation: &CancellationToken,
    ) -> Result<SegmentDecode, StreamSubtitleError> {
        self.fetch_calls.fetch_add(1, Ordering::AcqRel);
        while !self.gate_open.load(Ordering::Acquire) {
            if cancellation.is_cancelled() {
                self.cancellation_seen.store(true, Ordering::Release);
                return Err(StreamSubtitleError::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if self.failing.load(Ordering::Acquire) {
            return Err(StreamSubtitleError::Network);
        }
        let address = segment
            .reference
            .downcast_ref::<SegmentAddress>()
            .ok_or(StreamSubtitleError::Malformed)?;
        let cues = self
            .segments
            .get(address.key as usize)
            .ok_or(StreamSubtitleError::NotFound)?;
        let start_ms = i64::try_from(segment.start.as_millis()).unwrap_or(i64::MAX);
        let end_ms = i64::try_from(segment.end().as_millis()).unwrap_or(i64::MAX);
        let _ = address.track;
        Ok(SegmentDecode::Ready(DecodedSegment {
            events: cues.iter().map(vtt_event).collect(),
            covered: Some(PlayerTimeSpan {
                start: SubtitleTime::from_millis(start_ms),
                end: SubtitleTime::from_millis(end_ms),
            }),
            segment_start: Some(SubtitleTime::from_millis(start_ms)),
        }))
    }
}

/// Событие субтитров через настоящий разбор WebVTT (как транспорт HLS).
fn vtt_event(cue: &CueSpec) -> SubtitleEvent {
    let vtt = format!(
        "WEBVTT\n\n{} --> {}\n{}\n",
        timestamp(cue.start_ms),
        timestamp(cue.end_ms),
        cue.text
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

/// Описание рендиции для теста.
pub(super) fn stream_track_info(
    index: u32,
    language: Option<&str>,
    name: &str,
    flags: StreamFlags,
) -> StreamSubtitleTrackInfo {
    StreamSubtitleTrackInfo {
        id: StreamSubtitleTrackId::new(index),
        language: language.map(Into::into),
        name: name.into(),
        forced: flags.forced,
        default: flags.default,
        autoselect: flags.autoselect,
    }
}

/// Флаги рендиции манифеста (именованные поля вместо трёх `bool` на месте вызова).
#[derive(Debug, Clone, Copy)]
pub(super) struct StreamFlags {
    pub(super) forced: bool,
    pub(super) default: bool,
    pub(super) autoselect: bool,
}

impl StreamFlags {
    /// Обычная рендиция с AUTOSELECT=YES.
    pub(super) const AUTO: Self = Self {
        forced: false,
        default: false,
        autoselect: true,
    };
}

/// Источник потока из транспорта.
pub(super) fn stream_source(transport: &Arc<FakeStreamTransport>) -> StreamSubtitleSource {
    StreamSubtitleSource::new(Arc::clone(transport) as Arc<dyn StreamSubtitleTransport>)
        .expect("fake transport has tracks")
}

/// Владелец с потоковым media (по умолчанию субтитры выключены), позиция плеера — `position`.
pub(super) fn stream_owner(
    config: &SubtitlesConfig,
    media: MediaInstanceId,
    source: StreamSubtitleSource,
    position: Duration,
) -> SubtitleOwner {
    let mut owner = owner_with_config(config);
    owner.begin_media(media, SubtitleMediaSource::Stream(source));
    observe_position(&mut owner, media, position, StreamTimelineKind::Vod);
    owner
}

/// Наблюдение за плеером: воспроизведение на `position`.
pub(super) fn observe_position(
    owner: &mut SubtitleOwner,
    media: MediaInstanceId,
    position: Duration,
    timeline: StreamTimelineKind,
) {
    owner.observe_stream_playback(
        current(media, 1),
        &StreamPlaybackObservation {
            position,
            seek_target: None,
            timeline,
        },
    );
}

/// Ждёт, пока ленте придёт хотя бы `cues` реплик.
pub(super) fn wait_for_cues(owner: &mut SubtitleOwner, cues: u64) {
    super::embedded_support::wait_until(owner, "stream cues", |owner| {
        owner.stream_feed_counters().cues_added >= cues
    });
}

/// Собирает уведомления потоковых субтитров, пока не пройдёт `duration` (в тестах «ровно одно»).
pub(super) fn collect_notices_for(
    owner: &mut SubtitleOwner,
    duration: Duration,
) -> Vec<StreamSubtitleNotice> {
    let deadline = Instant::now() + duration;
    let mut notices = Vec::new();
    while Instant::now() < deadline {
        owner.poll_jobs();
        notices.extend(owner.take_stream_notices());
        std::thread::sleep(Duration::from_millis(2));
    }
    notices
}

/// Сколько реплик в таймлайне показываемой дорожки.
pub(super) fn displayed_cue_count(owner: &SubtitleOwner) -> usize {
    displayed_track(owner).map_or(0, |track| track.timeline().len())
}

/// Тексты реплик показываемой дорожки, активные на `pts_millis`.
pub(super) fn displayed_active_texts(owner: &SubtitleOwner, pts_millis: u64) -> Vec<String> {
    let Some(track) = displayed_track(owner) else {
        return Vec::new();
    };
    let lookup = track.lookup_time_for(Duration::from_millis(pts_millis));
    let mut ids = Vec::new();
    track.timeline().collect_active_at(lookup, &mut ids);
    ids.into_iter()
        .filter_map(|id| track.timeline().event(id))
        .filter_map(|event| match event.content() {
            subtitle_core::SubtitleContent::Text(cue) => Some(cue.text.plain_text()),
            _ => None,
        })
        .collect()
}

fn displayed_track(
    owner: &SubtitleOwner,
) -> Option<&super::super::active_track::ActiveSubtitleTrack> {
    let media = owner.media.as_ref()?;
    match &media.tracks.get(media.display_track_id()?)?.load {
        TrackLoadState::Ready(track) => Some(track),
        _ => None,
    }
}

/// Источник реплик показываемой дорожки (меняется при пересборке истории live).
pub(super) fn displayed_source_id(owner: &SubtitleOwner) -> Option<subtitle_raster::CueSourceId> {
    displayed_track(owner).map(super::super::active_track::ActiveSubtitleTrack::source_id)
}
