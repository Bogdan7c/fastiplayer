use std::time::Duration;

use codec_core::{ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction};
use media_core::TrackInfo;

use crate::{
    AudioBufferSnapshot, BackendSnapshot, FrameCounters, PlayerSnapshot, QueueSnapshot,
    TexturePoolSnapshot, TrackSelectionSnapshot, TrackSummarySnapshot, VideoFrameSnapshot,
};

use super::PlayerSession;

/// Собирает актуальный read-model snapshot из immutable session state.
pub(super) fn build_snapshot(
    session: &PlayerSession,
    frame_counters: FrameCounters,
) -> PlayerSnapshot {
    PlayerSnapshotBuilder::new(session, frame_counters).build()
}

/// Read-only builder, который не владеет session state и не меняет lifecycle pipeline-а.
struct PlayerSnapshotBuilder<'session> {
    /// Session остаётся единственным владельцем playback state и pipeline данных.
    session: &'session PlayerSession,

    /// Runtime frame counters приходят от shell/worker boundary и только подмешиваются в read model.
    frame_counters: FrameCounters,
}

impl<'session> PlayerSnapshotBuilder<'session> {
    /// Создаёт builder поверх immutable session reference.
    fn new(session: &'session PlayerSession, frame_counters: FrameCounters) -> Self {
        Self {
            session,
            frame_counters,
        }
    }

    /// Собирает полный `PlayerSnapshot`, сохраняя прежнее содержимое и порядок overrides.
    fn build(&self) -> PlayerSnapshot {
        let mut snapshot = self.session.snapshot.clone();
        snapshot.playback_state = self.session.playback_state();
        snapshot.source_label = self.source_label();
        snapshot.media_title = self.media_title();
        snapshot.selected_tracks = self.track_selection_snapshot();
        snapshot.tracks = self.track_summary_snapshot();
        snapshot.active_backend = self.backend_snapshot();
        snapshot.current_video_frame = self.current_video_frame_snapshot();
        snapshot.render_generation = self.session.pipeline.render_generation;
        snapshot.video_frame_duration_estimate =
            self.session.pipeline.video_frame_duration_estimate();
        snapshot.audio_buffer = self.audio_buffer_snapshot();
        snapshot.queues = self.queue_snapshot();
        snapshot.frame_counters = self.frame_counters;
        snapshot.diagnostics = self
            .session
            .diagnostics
            .snapshot_with_queues(self.session.diagnostic_queue_depths());
        snapshot
    }

    /// Формирует метку источника без раскрытия mutable demuxer state наружу.
    fn source_label(&self) -> Option<String> {
        self.session
            .pipeline
            .source_file_path()
            .map(|file_path| file_path.display().to_string())
            .or_else(|| self.session.pipeline.source_label().map(ToOwned::to_owned))
            .or_else(|| self.session.snapshot.source_label.clone())
    }

    /// Возвращает имя файла или streaming label как текущий media title.
    fn media_title(&self) -> Option<String> {
        self.session
            .pipeline
            .source_file_path()
            .and_then(|file_path| file_path.file_name())
            .map(|file_name| file_name.to_string_lossy().into_owned())
            .or_else(|| self.session.snapshot.media_title.clone())
    }

    /// Собирает snapshot выбранных tracks.
    fn track_selection_snapshot(&self) -> TrackSelectionSnapshot {
        TrackSelectionSnapshot {
            video_track: self.session.pipeline.selected_video_track_id(),
            audio_track: self.session.pipeline.selected_audio_track_id(),
            subtitle_track: self.session.snapshot.selected_tracks.subtitle_track,
        }
    }

    /// Собирает compact track metadata для UI.
    fn track_summary_snapshot(&self) -> Vec<TrackSummarySnapshot> {
        let mut track_summaries = Vec::with_capacity(self.session.pipeline.track_count());

        for track in self.session.pipeline.tracks() {
            track_summaries.push(TrackSummarySnapshot {
                id: track.id,
                kind: track.kind,
                codec_id: track.codec_id.clone(),
                sample_rate: track.sample_rate,
                channels: track.channels,
                video_color_summary: video_color_summary(track),
            });
        }

        track_summaries
    }

    /// Собирает snapshot активного backend и texture pool.
    fn backend_snapshot(&self) -> BackendSnapshot {
        BackendSnapshot {
            name: Some(self.session.pipeline.video_backend_name().to_string()),
            texture_pool: self.texture_pool_snapshot(),
        }
    }

    /// Конвертирует backend resource stats в core snapshot без передачи backend handles.
    fn texture_pool_snapshot(&self) -> Option<TexturePoolSnapshot> {
        self.session
            .pipeline
            .video_decoder_resource_snapshot()
            .map(|texture_stats| TexturePoolSnapshot {
                capacity: texture_stats.capacity,
                slots: texture_stats.slots,
                in_use: texture_stats.in_use,
                free_surfaces: texture_stats.free_surfaces,
                waiting_gpu_completion: texture_stats.waiting_gpu_completion,
                waiting_decoder_reuse: texture_stats.waiting_decoder_reuse,
                import_failures: texture_stats.import_failures,
                imports_created: texture_stats.imports_created,
                imports_reused: texture_stats.imports_reused,
                imports_replaced: texture_stats.imports_replaced,
            })
    }

    /// Описывает текущий кадр без передачи renderer-owned ресурсов.
    fn current_video_frame_snapshot(&self) -> Option<VideoFrameSnapshot> {
        self.session
            .pipeline
            .present_video_frame()
            .map(|present_frame| VideoFrameSnapshot {
                render_generation: self.session.pipeline.render_generation,
                handle: present_frame.texture_handle.0,
                pts: present_frame.pts,
                width: present_frame.width,
                height: present_frame.height,
                render_width: present_frame.render_width,
                render_height: present_frame.render_height,
                format: present_frame.format,
                memory_path: present_frame.memory_path,
            })
    }

    /// Собирает snapshot audio buffer.
    fn audio_buffer_snapshot(&self) -> AudioBufferSnapshot {
        AudioBufferSnapshot {
            level: self
                .session
                .audio_buffer_level_ms()
                .and_then(|level_ms| optional_duration_from_seconds(level_ms / 1000.0)),
            underruns: self.session.pipeline.audio_clock_underrun_callbacks(),
        }
    }

    /// Собирает snapshot очередей без раскрытия их содержимого.
    fn queue_snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            pending_audio_packets: self.session.pipeline.pending_audio_packet_len(),
            pending_video_packets: self.session.pipeline.pending_video_packet_len(),
            decoded_video_frames: self.session.pipeline.video_present_queue_len(),
        }
    }
}

/// Формирует compact color summary для media info panel.
fn video_color_summary(track: &TrackInfo) -> Option<String> {
    let color = track.video.as_ref()?.color.as_ref()?;
    Some(format!(
        "{} {} {} {}",
        display_primaries(color.primaries),
        display_transfer(color.transfer),
        display_matrix(color.matrix),
        display_range(color.range)
    ))
}

/// Возвращает stable label для primaries.
fn display_primaries(primaries: ColorPrimaries) -> &'static str {
    match primaries {
        ColorPrimaries::Bt709 => "BT.709",
        ColorPrimaries::Bt2020 => "BT.2020",
        ColorPrimaries::Smpte170m => "SMPTE 170M",
        ColorPrimaries::Bt470Bg => "BT.470BG",
        ColorPrimaries::Unknown => "primaries unknown",
    }
}

/// Возвращает stable label для transfer function.
fn display_transfer(transfer: TransferFunction) -> &'static str {
    match transfer {
        TransferFunction::Bt709 => "BT.709",
        TransferFunction::Srgb => "sRGB",
        TransferFunction::Pq => "PQ",
        TransferFunction::Hlg => "HLG",
        TransferFunction::Unknown => "transfer unknown",
    }
}

/// Возвращает stable label для matrix coefficients.
fn display_matrix(matrix: MatrixCoefficients) -> &'static str {
    match matrix {
        MatrixCoefficients::Bt601 => "BT.601",
        MatrixCoefficients::Bt709 => "BT.709 matrix",
        MatrixCoefficients::Bt2020 => "BT.2020 matrix",
        MatrixCoefficients::Unknown => "matrix unknown",
    }
}

/// Возвращает stable label для range.
fn display_range(range: ColorRange) -> &'static str {
    match range {
        ColorRange::Limited => "limited",
        ColorRange::Full => "full",
        ColorRange::Unknown => "range unknown",
    }
}

/// Безопасно создаёт `Duration` только из finite и неотрицательных секунд.
fn optional_duration_from_seconds(seconds: f64) -> Option<Duration> {
    Duration::try_from_secs_f64(seconds).ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use bytes::Bytes;
    use media_core::{DemuxSeekResult, Demuxer, MediaTime, TrackKind};

    use crate::pipeline::{PendingAudioPacket, PendingVideoPacket};
    use crate::{FrameCounters, TrackId};

    use super::*;

    #[test]
    fn builder_does_not_mutate_base_session_snapshot() {
        let mut session = PlayerSession::default();
        session.snapshot.source_label = Some("fallback source".to_owned());
        session.pipeline.install_opened_media(
            Box::new(SnapshotFakeDemuxer::empty()),
            None,
            Some("pipeline source".to_owned()),
            Vec::new(),
        );

        let snapshot_before_build = session.snapshot.clone();
        let frame_counters = FrameCounters {
            presented: 3,
            dropped: 2,
            repeated: 1,
        };

        let snapshot = build_snapshot(&session, frame_counters);

        assert_eq!(snapshot.source_label.as_deref(), Some("pipeline source"));
        assert_eq!(snapshot.frame_counters, frame_counters);
        assert_eq!(session.snapshot, snapshot_before_build);
    }

    #[test]
    fn builder_preserves_file_path_label_and_title_fallback() {
        let mut session = PlayerSession::default();
        session.snapshot.source_label = Some("fallback source".to_owned());
        session.snapshot.media_title = Some("fallback title".to_owned());
        session.pipeline.install_opened_media(
            Box::new(SnapshotFakeDemuxer::empty()),
            Some(PathBuf::from("/tmp/sample.webm")),
            Some("streaming source".to_owned()),
            Vec::new(),
        );

        let snapshot = build_snapshot(&session, FrameCounters::default());

        assert_eq!(snapshot.source_label.as_deref(), Some("/tmp/sample.webm"));
        assert_eq!(snapshot.media_title.as_deref(), Some("sample.webm"));
    }

    #[test]
    fn builder_preserves_track_snapshot_from_pipeline_getter() {
        let mut session = PlayerSession::default();
        let track_infos = vec![
            snapshot_track(TrackId::new(1), TrackKind::Video, "V_VP9"),
            snapshot_track(TrackId::new(2), TrackKind::Audio, "A_OPUS"),
        ];
        session.pipeline.install_opened_media(
            Box::new(SnapshotFakeDemuxer::new(track_infos.clone())),
            None,
            Some("pipeline source".to_owned()),
            track_infos,
        );

        let snapshot = build_snapshot(&session, FrameCounters::default());

        assert_eq!(snapshot.tracks.len(), 2);
        assert_eq!(snapshot.tracks[0].id, TrackId::new(1));
        assert_eq!(snapshot.tracks[0].kind, TrackKind::Video);
        assert_eq!(snapshot.tracks[0].codec_id, "V_VP9");
        assert_eq!(snapshot.tracks[1].id, TrackId::new(2));
        assert_eq!(snapshot.tracks[1].kind, TrackKind::Audio);
        assert_eq!(snapshot.tracks[1].codec_id, "A_OPUS");
    }

    #[test]
    fn builder_reads_queue_counters_without_draining_pipeline() {
        let mut session = PlayerSession::default();
        let generation = session.pipeline.seek_generation();

        session
            .pipeline
            .enqueue_pending_audio_packet(PendingAudioPacket::new(
                TrackId::new(1),
                Duration::from_millis(10),
                generation,
                Bytes::new(),
            ));
        session
            .pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(2),
                Duration::from_millis(20),
                generation,
                Bytes::new(),
                true,
            ));

        let snapshot = build_snapshot(&session, FrameCounters::default());

        assert_eq!(snapshot.queues.pending_audio_packets, 1);
        assert_eq!(snapshot.queues.pending_video_packets, 1);
        assert_eq!(snapshot.diagnostics.queues.pending_audio_packets, 1);
        assert_eq!(snapshot.diagnostics.queues.pending_video_packets, 1);
        assert_eq!(session.pipeline.pending_audio_packet_len(), 1);
        assert_eq!(session.pipeline.pending_video_packet_len(), 1);
    }

    /// Fake demuxer для snapshot tests: builder не читает packets, но pipeline требует handle.
    struct SnapshotFakeDemuxer {
        /// Metadata tracks, которые возвращает neutral demux contract.
        track_infos: Vec<TrackInfo>,
    }

    impl SnapshotFakeDemuxer {
        /// Создаёт fake demuxer без tracks для source fallback сценариев.
        fn empty() -> Self {
            Self::new(Vec::new())
        }

        /// Создаёт fake demuxer с явным immutable tracks snapshot.
        fn new(track_infos: Vec<TrackInfo>) -> Self {
            Self { track_infos }
        }
    }

    impl Demuxer for SnapshotFakeDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &self.track_infos
        }

        fn duration(&self) -> Option<Duration> {
            None
        }

        fn next_packet(&mut self) -> anyhow::Result<Option<media_core::Packet>> {
            Ok(None)
        }

        fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            Ok(DemuxSeekResult {
                requested_position: MediaTime::from_duration(timestamp),
                actual_position: MediaTime::from_duration(timestamp),
                actual_track_timestamp: None,
            })
        }
    }

    /// Создаёт минимальный track metadata для проверки snapshot mapping.
    fn snapshot_track(track_id: TrackId, kind: TrackKind, codec_id: &str) -> TrackInfo {
        TrackInfo {
            id: track_id,
            kind,
            codec_id: codec_id.to_owned(),
            codec_private: None,
            time_base: media_core::TimeBase::new(1, 1_000),
            duration: Some(Duration::from_secs(30)),
            sample_rate: (kind == TrackKind::Audio).then_some(48_000),
            channels: (kind == TrackKind::Audio).then_some(2),
            video: None,
        }
    }
}
