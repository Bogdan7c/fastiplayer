use std::time::Duration;

use media_core::{MediaDuration, MediaTime, TrackKind};
use rustiplayer_config::NetworkConfig;

use crate::{PlayerTickPacket, SeekMode, TrackId};

/// Runtime-настройки фонового indexer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackgroundIndexerConfig {
    /// IO budget фонового demux/byte scan в bytes/sec.
    pub io_budget_bytes_per_sec: u64,
}

impl BackgroundIndexerConfig {
    /// Собирает indexer config из validated network config.
    #[must_use]
    pub fn from_network_config(network_config: &NetworkConfig) -> Self {
        Self {
            io_budget_bytes_per_sec: network_config
                .indexer_io_budget_mb_per_sec
                .saturating_mul(1024)
                .saturating_mul(1024),
        }
    }
}

impl Default for BackgroundIndexerConfig {
    /// Использует documented network defaults без дублирования числовых IO constants.
    fn default() -> Self {
        Self::from_network_config(&NetworkConfig::default())
    }
}

/// Причина паузы фонового indexer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexPauseReason {
    /// Пользователь активно scrubbing timeline.
    ActiveScrub,

    /// Playback buffer ниже безопасного уровня.
    LowPlaybackBuffer,

    /// Decode/render queues находятся под высокой нагрузкой.
    HighDecodeOrRenderLoad,

    /// Worker/session явно завершает работу.
    Shutdown,
}

/// Давление playback pipeline, под которым indexer должен уступить ресурсы.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct IndexPressureSnapshot {
    /// Сейчас активен timeline scrub.
    pub active_scrub: bool,

    /// Playback buffer ниже low watermark.
    pub low_playback_buffer: bool,

    /// Decode/render path близок к backpressure.
    pub high_decode_render_load: bool,

    /// Worker/session завершает работу.
    pub shutdown_requested: bool,
}

impl IndexPressureSnapshot {
    /// Возвращает приоритетную причину паузы.
    #[must_use]
    pub const fn pause_reason(self) -> Option<IndexPauseReason> {
        if self.shutdown_requested {
            Some(IndexPauseReason::Shutdown)
        } else if self.active_scrub {
            Some(IndexPauseReason::ActiveScrub)
        } else if self.low_playback_buffer {
            Some(IndexPauseReason::LowPlaybackBuffer)
        } else if self.high_decode_render_load {
            Some(IndexPauseReason::HighDecodeOrRenderLoad)
        } else {
            None
        }
    }
}

/// Одна metadata-точка keyframe/time index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundIndexEntry {
    /// Track id внутри контейнера.
    pub track_id: TrackId,

    /// Время packet/sample на media timeline.
    pub time: MediaTime,

    /// Byte offset, если demux/source adapter его умеет отдавать.
    pub byte_offset: Option<u64>,

    /// Признак keyframe.
    pub keyframe: bool,
}

/// Результат выбора demux seek target-а по runtime index-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedDemuxSeekTarget {
    /// Позиция, с которой demuxer должен начать pre-roll.
    pub position: MediaTime,

    /// Safe source byte offset для backend-а, если index его знает.
    pub byte_offset: Option<u64>,
}

/// Прогресс построения index-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexProgressSnapshot {
    /// Последняя просканированная media-позиция.
    pub indexed_until: Option<MediaTime>,

    /// Длительность media, если известна.
    pub duration: Option<MediaDuration>,

    /// Количество накопленных metadata entries.
    pub entries: u64,

    /// `true`, если scan дошёл до EOF/container end.
    pub complete: bool,
}

/// Диагностика seek latency и target-vs-actual.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeekAccessDiagnostics {
    /// Latency последнего seek commit-а.
    pub last_seek_latency: Option<Duration>,

    /// Последняя requested target position.
    pub last_seek_target: Option<MediaTime>,

    /// Последняя actual container position после demux seek.
    pub last_seek_actual: Option<MediaTime>,
}

/// Diagnostics cache/index/seek path-а для telemetry panel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexDiagnostics {
    /// Прогресс фонового или observed index build.
    pub progress: IndexProgressSnapshot,

    /// Активна ли пауза indexer-а.
    pub paused: bool,

    /// Причина текущей паузы.
    pub pause_reason: Option<IndexPauseReason>,

    /// Cache hits из source layer, когда shell прокидывает их в player diagnostics.
    pub cache_hits: u64,

    /// Cache misses из source layer.
    pub cache_misses: u64,

    /// HTTP Range request count.
    pub range_requests: u64,

    /// Seek latency и target-vs-actual.
    pub seek: SeekAccessDiagnostics,

    /// Количество stale job updates.
    pub stale_jobs: u64,

    /// Количество отменённых или superseded jobs.
    pub cancelled_or_superseded_jobs: u64,

    /// Количество timeout-ов index/seek path-а.
    pub timeouts: u64,
}

/// Лёгкий metadata indexer без decoder/audio/render ресурсов.
#[derive(Debug, Clone)]
pub struct BackgroundIndexer {
    /// Настройки budget-ов indexer-а.
    config: BackgroundIndexerConfig,

    /// Активное поколение job-а.
    active_generation: u64,

    /// Identity текущего source-а.
    active_source_key: Option<String>,

    /// Накопленные metadata entries.
    entries: Vec<BackgroundIndexEntry>,

    /// Текущие diagnostics counters.
    diagnostics: IndexDiagnostics,
}

impl BackgroundIndexer {
    /// Создаёт indexer с config из validated app config.
    #[must_use]
    pub fn new(config: BackgroundIndexerConfig) -> Self {
        Self {
            config,
            active_generation: 0,
            active_source_key: None,
            entries: Vec::new(),
            diagnostics: IndexDiagnostics::default(),
        }
    }

    /// Возвращает текущую конфигурацию.
    #[must_use]
    pub const fn config(&self) -> BackgroundIndexerConfig {
        self.config
    }

    /// Возвращает активное поколение job-а.
    #[must_use]
    pub const fn active_generation(&self) -> u64 {
        self.active_generation
    }

    /// Возвращает ключ активного source-а, если job запущен.
    #[must_use]
    pub fn active_source_key(&self) -> Option<&str> {
        self.active_source_key.as_deref()
    }

    /// Возвращает immutable diagnostics snapshot.
    #[must_use]
    pub const fn diagnostics(&self) -> IndexDiagnostics {
        self.diagnostics
    }

    /// Начинает новый index job и supersede-ит предыдущий.
    pub fn start_job(
        &mut self,
        source_key: impl Into<String>,
        duration: Option<MediaDuration>,
        initial_entries: Vec<BackgroundIndexEntry>,
    ) {
        if self.active_source_key.is_some() {
            self.diagnostics.cancelled_or_superseded_jobs = self
                .diagnostics
                .cancelled_or_superseded_jobs
                .saturating_add(1);
        }

        self.active_generation = self.active_generation.saturating_add(1);
        self.active_source_key = Some(source_key.into());
        self.entries = initial_entries;
        self.diagnostics.progress = IndexProgressSnapshot {
            indexed_until: self.entries.iter().map(|entry| entry.time).max(),
            duration,
            entries: self.entries.len() as u64,
            complete: false,
        };
        self.diagnostics.paused = false;
        self.diagnostics.pause_reason = None;
    }

    /// Отменяет активный job без сброса diagnostics counters.
    pub fn cancel_active_job(&mut self) {
        if self.active_source_key.take().is_some() {
            self.diagnostics.cancelled_or_superseded_jobs = self
                .diagnostics
                .cancelled_or_superseded_jobs
                .saturating_add(1);
        }
        self.entries.clear();
        self.diagnostics.progress = IndexProgressSnapshot::default();
        self.diagnostics.paused = false;
        self.diagnostics.pause_reason = None;
    }

    /// Применяет pressure policy: indexer уступает playback при любом активном давлении.
    pub fn apply_pressure(&mut self, pressure: IndexPressureSnapshot) {
        let pause_reason = pressure.pause_reason();
        self.diagnostics.paused = pause_reason.is_some();
        self.diagnostics.pause_reason = pause_reason;
    }

    /// Записывает metadata entry из активного job-а.
    pub fn record_entry(&mut self, generation: u64, entry: BackgroundIndexEntry) {
        if generation != self.active_generation || self.active_source_key.is_none() {
            self.diagnostics.stale_jobs = self.diagnostics.stale_jobs.saturating_add(1);
            return;
        }

        self.diagnostics.progress.indexed_until = Some(
            self.diagnostics
                .progress
                .indexed_until
                .map_or(entry.time, |time| {
                    if entry.time > time { entry.time } else { time }
                }),
        );
        self.entries.push(entry);
        self.diagnostics.progress.entries = self.entries.len() as u64;
    }

    /// Записывает video packet metadata, уже прочитанную demuxer-ом.
    pub fn record_tick_packet(&mut self, packet: &PlayerTickPacket) {
        if packet.kind != TrackKind::Video {
            return;
        }

        self.record_video_packet(
            packet.track_id,
            packet.pts,
            packet.byte_offset,
            packet.keyframe,
        );
    }

    /// Записывает metadata video packet-а без codec bytes.
    pub fn record_video_packet(
        &mut self,
        track_id: TrackId,
        pts: Duration,
        byte_offset: Option<u64>,
        keyframe: bool,
    ) {
        if self.active_source_key.is_none() {
            return;
        }

        let entry = BackgroundIndexEntry {
            track_id,
            time: MediaTime::from_duration(pts),
            byte_offset,
            keyframe,
        };
        self.record_entry(self.active_generation, entry);
    }

    /// Отмечает завершение scan-а.
    pub fn mark_complete(&mut self, generation: u64) {
        if generation != self.active_generation || self.active_source_key.is_none() {
            self.diagnostics.stale_jobs = self.diagnostics.stale_jobs.saturating_add(1);
            return;
        }

        self.diagnostics.progress.complete = true;
    }

    /// Записывает cache diagnostics, пришедшие из source layer.
    pub fn update_cache_counters(&mut self, hits: u64, misses: u64) {
        self.diagnostics.cache_hits = hits;
        self.diagnostics.cache_misses = misses;
    }

    /// Записывает range diagnostics, пришедшие из source layer.
    pub fn update_range_counters(&mut self, range_requests: u64, timeouts: u64) {
        self.diagnostics.range_requests = range_requests;
        self.diagnostics.timeouts = timeouts;
    }

    /// Записывает successful seek diagnostics.
    pub fn note_seek_completed(&mut self, latency: Duration, target: MediaTime, actual: MediaTime) {
        self.diagnostics.seek = SeekAccessDiagnostics {
            last_seek_latency: Some(latency),
            last_seek_target: Some(target),
            last_seek_actual: Some(actual),
        };
    }

    /// Записывает timeout seek/index path-а.
    pub fn note_timeout(&mut self) {
        self.diagnostics.timeouts = self.diagnostics.timeouts.saturating_add(1);
    }

    /// Возвращает ближайший keyframe не позже target для выбранного track-а.
    #[must_use]
    pub fn nearest_keyframe_before(
        &self,
        track_id: TrackId,
        target: MediaTime,
    ) -> Option<MediaTime> {
        self.nearest_keyframe_entry_before(track_id, target)
            .map(|entry| entry.time)
    }

    /// Возвращает ближайшую keyframe entry не позже target для выбранного track-а.
    #[must_use]
    pub fn nearest_keyframe_entry_before(
        &self,
        track_id: TrackId,
        target: MediaTime,
    ) -> Option<&BackgroundIndexEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.track_id == track_id && entry.keyframe && entry.time <= target)
            .max_by_key(|entry| entry.time)
    }

    /// Выбирает demux seek target с учётом накопленного keyframe index-а.
    #[must_use]
    pub fn resolve_demux_seek_target(
        &self,
        mode: SeekMode,
        track_id: Option<TrackId>,
        target: MediaTime,
    ) -> MediaTime {
        if mode != SeekMode::KeyframeBefore {
            return target;
        }

        track_id
            .and_then(|track_id| self.nearest_keyframe_before(track_id, target))
            .unwrap_or(target)
    }

    /// Выбирает demux seek target и optional byte-offset hint.
    #[must_use]
    pub fn resolve_demux_seek_target_with_hint(
        &self,
        mode: SeekMode,
        track_id: Option<TrackId>,
        target: MediaTime,
    ) -> ResolvedDemuxSeekTarget {
        if mode != SeekMode::KeyframeBefore {
            return ResolvedDemuxSeekTarget {
                position: target,
                byte_offset: None,
            };
        }

        track_id
            .and_then(|track_id| self.nearest_keyframe_entry_before(track_id, target))
            .map(|entry| ResolvedDemuxSeekTarget {
                position: entry.time,
                byte_offset: entry.byte_offset,
            })
            .unwrap_or(ResolvedDemuxSeekTarget {
                position: target,
                byte_offset: None,
            })
    }
}

impl Default for BackgroundIndexer {
    /// Создаёт indexer с documented default budget.
    fn default() -> Self {
        Self::new(BackgroundIndexerConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет priority pause reasons.
    #[test]
    fn indexer_pauses_under_pressure() {
        let mut indexer = BackgroundIndexer::default();

        indexer.apply_pressure(IndexPressureSnapshot {
            active_scrub: true,
            low_playback_buffer: true,
            high_decode_render_load: true,
            shutdown_requested: false,
        });

        assert!(indexer.diagnostics().paused);
        assert_eq!(
            indexer.diagnostics().pause_reason,
            Some(IndexPauseReason::ActiveScrub)
        );

        indexer.apply_pressure(IndexPressureSnapshot {
            shutdown_requested: true,
            ..IndexPressureSnapshot::default()
        });

        assert_eq!(
            indexer.diagnostics().pause_reason,
            Some(IndexPauseReason::Shutdown)
        );
    }

    /// Проверяет stale job counter.
    #[test]
    fn stale_generation_update_is_counted_and_ignored() {
        let mut indexer = BackgroundIndexer::default();
        indexer.start_job("source-a", Some(MediaDuration::from_secs(10)), Vec::new());

        indexer.record_entry(999, test_entry(1, 1, true));

        assert_eq!(indexer.diagnostics().stale_jobs, 1);
        assert_eq!(indexer.diagnostics().progress.entries, 0);
    }

    /// Проверяет superseded/cancelled counter.
    #[test]
    fn starting_new_job_counts_superseded_job() {
        let mut indexer = BackgroundIndexer::default();

        indexer.start_job("source-a", None, Vec::new());
        indexer.start_job("source-b", None, Vec::new());

        assert_eq!(indexer.diagnostics().cancelled_or_superseded_jobs, 1);
    }

    /// Проверяет keyframe lookup для keyframe-before seek.
    #[test]
    fn nearest_keyframe_before_returns_latest_keyframe_not_after_target() {
        let mut indexer = BackgroundIndexer::default();
        indexer.start_job(
            "source-a",
            Some(MediaDuration::from_secs(20)),
            vec![
                test_entry(1, 0, true),
                test_entry(1, 5, true),
                test_entry(1, 8, false),
                test_entry(1, 12, true),
            ],
        );

        let keyframe = indexer.nearest_keyframe_before(TrackId::new(1), MediaTime::from_secs(9));

        assert_eq!(keyframe, Some(MediaTime::from_secs(5)));
    }

    /// Проверяет, что keyframe/time index отдаёт byte-offset вместе с demux target.
    #[test]
    fn resolve_demux_seek_target_preserves_byte_offset_hint() {
        let mut indexer = BackgroundIndexer::default();
        indexer.start_job(
            "source-a",
            Some(MediaDuration::from_secs(20)),
            vec![
                test_entry(1, 0, true),
                test_entry_with_offset(1, 5, true, 4096),
                test_entry_with_offset(1, 12, true, 8192),
            ],
        );

        let target = indexer.resolve_demux_seek_target_with_hint(
            SeekMode::KeyframeBefore,
            Some(TrackId::new(1)),
            MediaTime::from_secs(9),
        );

        assert_eq!(
            target,
            ResolvedDemuxSeekTarget {
                position: MediaTime::from_secs(5),
                byte_offset: Some(4096),
            }
        );
    }

    /// Проверяет cache/range/seek diagnostics counters.
    #[test]
    fn diagnostics_record_cache_range_seek_and_timeout_counters() {
        let mut indexer = BackgroundIndexer::default();

        indexer.update_cache_counters(2, 3);
        indexer.update_range_counters(4, 1);
        indexer.note_seek_completed(
            Duration::from_millis(15),
            MediaTime::from_secs(10),
            MediaTime::from_secs(8),
        );
        indexer.note_timeout();

        let diagnostics = indexer.diagnostics();
        assert_eq!(diagnostics.cache_hits, 2);
        assert_eq!(diagnostics.cache_misses, 3);
        assert_eq!(diagnostics.range_requests, 4);
        assert_eq!(diagnostics.timeouts, 2);
        assert_eq!(
            diagnostics.seek.last_seek_latency,
            Some(Duration::from_millis(15))
        );
        assert_eq!(
            diagnostics.seek.last_seek_target,
            Some(MediaTime::from_secs(10))
        );
        assert_eq!(
            diagnostics.seek.last_seek_actual,
            Some(MediaTime::from_secs(8))
        );
    }

    /// Создаёт test entry.
    fn test_entry(track_id: u32, seconds: u64, keyframe: bool) -> BackgroundIndexEntry {
        BackgroundIndexEntry {
            track_id: TrackId::new(track_id),
            time: MediaTime::from_secs(seconds),
            byte_offset: None,
            keyframe,
        }
    }

    /// Создаёт test entry с byte offset.
    fn test_entry_with_offset(
        track_id: u32,
        seconds: u64,
        keyframe: bool,
        byte_offset: u64,
    ) -> BackgroundIndexEntry {
        BackgroundIndexEntry {
            byte_offset: Some(byte_offset),
            ..test_entry(track_id, seconds, keyframe)
        }
    }
}
