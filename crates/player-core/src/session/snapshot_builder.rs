use std::time::Duration;

use codec_core::{ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction};
use media_core::TrackInfo;

use crate::{
    AudioBufferSnapshot, BackendSnapshot, PlayerSnapshot, QueueSnapshot, TexturePoolSnapshot,
    TrackSelectionSnapshot, TrackSummarySnapshot, VideoFrameSnapshot,
};

use super::PlayerSession;

/// Собирает актуальный read-model snapshot из immutable session state.
pub(super) fn build_snapshot(session: &PlayerSession) -> PlayerSnapshot {
    PlayerSnapshotBuilder::new(session).build()
}

/// Read-only builder, который не владеет session state и не меняет lifecycle pipeline-а.
struct PlayerSnapshotBuilder<'session> {
    /// Session остаётся единственным владельцем playback state и pipeline данных.
    session: &'session PlayerSession,
}

impl<'session> PlayerSnapshotBuilder<'session> {
    /// Создаёт builder поверх immutable session reference.
    fn new(session: &'session PlayerSession) -> Self {
        Self { session }
    }

    /// Собирает полный `PlayerSnapshot`.
    ///
    /// Снимок строится литералом без `..`, поэтому новое поле `PlayerSnapshot`
    /// не скомпилируется, пока не решено, откуда оно берётся. Раньше сборка
    /// начиналась с глубокого клона `session.snapshot` и почти все тяжёлые поля
    /// (tracks, media_info, строки) тут же перезаписывались: двойная работа на
    /// каждом тике. Теперь из `session.snapshot` клонируются только поля, которые
    /// владеет сама session (команды/события), остальные строятся из pipeline.
    fn build(&self) -> PlayerSnapshot {
        let base = &self.session.snapshot;
        PlayerSnapshot {
            // --- Поля, которыми владеет session.snapshot (pipeline их не знает) ---
            media_instance_id: base.media_instance_id,
            duration: base.duration,
            current_position: base.current_position,
            timeline: base.timeline,
            volume: base.volume,
            muted: base.muted,
            playback_rate: base.playback_rate,
            available_qualities: base.available_qualities.clone(),
            last_error: base.last_error.clone(),
            capability_summary: base.capability_summary.clone(),
            // --- Поля, собираемые из живого состояния pipeline/session ---
            playback_state: self.session.playback_state(),
            source_label: self.source_label(),
            media_title: self.media_title(),
            media_info: self.media_info_snapshot(),
            selected_tracks: self.track_selection_snapshot(),
            tracks: self.track_summary_snapshot(),
            active_backend: self.backend_snapshot(),
            current_video_frame: self.current_video_frame_snapshot(),
            subtitle_read_run: self.session.subtitle_feed.current_read_run(),
            render_generation: self.session.pipeline.render_generation(),
            video_frame_duration_estimate: self.session.pipeline.video_frame_duration_estimate(),
            audio_buffer: self.audio_buffer_snapshot(),
            queues: self.queue_snapshot(),
            diagnostics: self.session.diagnostics_snapshot(),
        }
    }

    /// Собирает информацию об источнике; длительность и seekability берёт из базового снимка.
    fn media_info_snapshot(&self) -> Option<crate::MediaInfoSnapshot> {
        let base = &self.session.snapshot;
        self.session
            .pipeline
            .media_source_info()
            .cloned()
            .map(|source| crate::MediaInfoSnapshot {
                source,
                metadata: self.session.pipeline.media_metadata().cloned(),
                duration: base.duration,
                seekable: base.timeline.not_seekable_reason.is_none(),
            })
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
                duration: track.duration,
                video: track.video.clone(),
                video_color_summary: video_color_summary(track),
                labels: track.labels.clone(),
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
                render_generation: self.session.pipeline.render_generation(),
                handle: present_frame.resource_handle.0,
                pts: present_frame.pts,
                width: present_frame.width,
                height: present_frame.height,
                render_width: present_frame.render_width,
                render_height: present_frame.render_height,
                format: present_frame.format(),
                memory_path: present_frame.memory_path(),
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

    use crate::TrackId;
    use crate::pipeline::{PendingAudioPacket, PendingVideoPacket};

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
        let snapshot = build_snapshot(&session);

        assert_eq!(snapshot.source_label.as_deref(), Some("pipeline source"));
        assert_eq!(session.snapshot, snapshot_before_build);
    }

    /// Поля, которыми владеет `session.snapshot` (команды/события), доходят до результата
    /// без изменений: сборка литералом не должна терять то, что раньше давал клон базы.
    #[test]
    fn builder_carries_session_owned_fields_from_base_snapshot() {
        let mut session = PlayerSession::default();
        let instance = crate::MediaInstanceId::new_unique();
        session.snapshot.media_instance_id = Some(instance);
        session.snapshot.volume = 0.4;
        session.snapshot.muted = true;
        session.snapshot.capability_summary = Some("caps".to_owned());
        session.snapshot.last_error = Some(crate::PlayerError::new(
            crate::PlayerErrorKind::RuntimeError,
            "boom",
        ));
        session
            .snapshot
            .set_timeline_position(MediaTime::from_duration(Duration::from_secs(7)));

        let snapshot = build_snapshot(&session);

        assert_eq!(snapshot.media_instance_id, Some(instance));
        assert!((snapshot.volume - 0.4).abs() < f32::EPSILON);
        assert!(snapshot.muted);
        assert_eq!(snapshot.capability_summary.as_deref(), Some("caps"));
        assert_eq!(snapshot.last_error, session.snapshot.last_error);
        assert_eq!(snapshot.current_position, Duration::from_secs(7));
        assert_eq!(snapshot.timeline, session.snapshot.timeline);
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

        let snapshot = build_snapshot(&session);

        assert_eq!(snapshot.source_label.as_deref(), Some("/tmp/sample.webm"));
        assert_eq!(snapshot.media_title.as_deref(), Some("sample.webm"));
    }

    #[test]
    fn builder_publishes_container_presentation_start_in_media_info() {
        let mut session = PlayerSession::default();
        session.pipeline.install_opened_media(
            Box::new(SnapshotFakeDemuxer::with_presentation_start(
                Duration::from_millis(1_400),
            )),
            Some(PathBuf::from("/tmp/recording.ts")),
            None,
            Vec::new(),
        );

        let snapshot = build_snapshot(&session);

        let start = snapshot
            .media_info
            .and_then(|info| info.metadata)
            .and_then(|metadata| metadata.container)
            .and_then(|container| container.presentation_start);
        assert_eq!(start, Some(Duration::from_millis(1_400)));
    }

    #[test]
    fn builder_preserves_render_generation_from_pipeline_getter() {
        let mut session = PlayerSession::default();
        session.pipeline.advance_render_generation();
        session.pipeline.advance_render_generation();

        let snapshot = build_snapshot(&session);

        assert_eq!(
            snapshot.render_generation,
            session.pipeline.render_generation()
        );
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

        let snapshot = build_snapshot(&session);

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
            .enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
                TrackId::new(1),
                Duration::from_millis(10),
                None,
                None,
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

        let snapshot = build_snapshot(&session);

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
        /// Metadata контейнера, которую demuxer публикует после открытия.
        metadata: Option<media_core::MediaMetadata>,
    }

    impl SnapshotFakeDemuxer {
        /// Создаёт fake demuxer без tracks для source fallback сценариев.
        fn empty() -> Self {
            Self::new(Vec::new())
        }

        /// Создаёт fake demuxer с явным immutable tracks snapshot.
        fn new(track_infos: Vec<TrackInfo>) -> Self {
            Self {
                track_infos,
                metadata: None,
            }
        }

        /// Demuxer, чей контейнер начинается не с нуля (запись MPEG-TS).
        fn with_presentation_start(start: Duration) -> Self {
            Self {
                track_infos: Vec::new(),
                metadata: Some(media_core::MediaMetadata {
                    container: Some(media_core::MediaContainerMetadata {
                        presentation_start: Some(start),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            }
        }
    }

    impl Demuxer for SnapshotFakeDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &self.track_infos
        }

        fn duration(&self) -> Option<Duration> {
            None
        }

        fn media_metadata(&self) -> Option<media_core::MediaMetadata> {
            self.metadata.clone()
        }

        fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
            Ok(media_core::DemuxReadEvent::EndOfStream)
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
            labels: media_core::TrackLabels::default(),
        }
    }
}
