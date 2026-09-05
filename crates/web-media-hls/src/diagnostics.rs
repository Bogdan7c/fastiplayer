//! Secret-safe telemetry выбранного manifest segment-а на HLS seek boundary.

use std::fmt::{self, Display, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use web_media_transport_api::SourceGeneration;

use crate::HlsVodSeekLandingPolicy;
use crate::seek::{HlsSeekAnchor, HlsSeekAnchorKind};

const HLS_MANIFEST_SELECTION_LOG_TARGET: &str = "fastiplayer::hls_manifest_selection";

/// Семантическая стадия HLS seek-а без смешения preview и final receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsManifestSeekDiagnosticPhase {
    InitialOpen,
    InitialRestore,
    Preview,
    FinalReceipt,
}

impl HlsManifestSeekDiagnosticPhase {
    const fn diagnostic_name(self) -> &'static str {
        match self {
            Self::InitialOpen => "initial_open",
            Self::InitialRestore => "initial_restore",
            Self::Preview => "preview",
            Self::FinalReceipt => "final_receipt",
        }
    }
}

/// Роль component-а в HLS-owned topology; значения не содержат manifest locator-ов.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsManifestComponentRole {
    Muxed,
    Video,
    Audio,
}

impl HlsManifestComponentRole {
    const fn diagnostic_name(self) -> &'static str {
        match self {
            Self::Muxed => "muxed",
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }

    pub(crate) fn from_tracks(tracks: &[media_core::TrackInfo]) -> anyhow::Result<Self> {
        let has_video = tracks
            .iter()
            .any(|track| track.kind == media_core::TrackKind::Video);
        let has_audio = tracks
            .iter()
            .any(|track| track.kind == media_core::TrackKind::Audio);
        match (has_video, has_audio) {
            (true, true) => Ok(Self::Muxed),
            (true, false) => Ok(Self::Video),
            (false, true) => Ok(Self::Audio),
            (false, false) => anyhow::bail!("HLS manifest marker component не содержит A/V tracks"),
        }
    }
}

static NEXT_MANIFEST_SELECTION_ID: AtomicU64 = AtomicU64::new(1);

/// HLS-local exact identity одной выбранной manifest записи; не является public request id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlsManifestSelectionId(u64);

impl HlsManifestSelectionId {
    fn allocate() -> Self {
        Self(NEXT_MANIFEST_SELECTION_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// Полное безопасное доказательство manifest selection без locator material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HlsManifestSegmentSeekMarker {
    phase: HlsManifestSeekDiagnosticPhase,
    component_role: HlsManifestComponentRole,
    manifest_selection_id: HlsManifestSelectionId,
    landing_policy: HlsVodSeekLandingPolicy,
    source_generation: SourceGeneration,
    requested_target: Duration,
    anchor: HlsSeekAnchor,
}

impl HlsManifestSegmentSeekMarker {
    /// Подготавливает immutable marker, который можно отложить до commit authority.
    pub(crate) fn new(
        phase: HlsManifestSeekDiagnosticPhase,
        component_role: HlsManifestComponentRole,
        landing_policy: HlsVodSeekLandingPolicy,
        source_generation: SourceGeneration,
        requested_target: Duration,
        anchor: HlsSeekAnchor,
    ) -> Self {
        Self {
            phase,
            component_role,
            manifest_selection_id: HlsManifestSelectionId::allocate(),
            landing_policy,
            source_generation,
            requested_target,
            anchor,
        }
    }

    /// Публикует marker только там, где вызывающий код уже получил commit authority.
    pub(crate) fn emit(self) {
        log::info!(target: HLS_MANIFEST_SELECTION_LOG_TARGET, "{self}");
    }

    /// Separate-audio prepare использует video landing как внутреннюю цель, но marker
    /// обязан сохранять исходный пользовательский target composite transaction-а.
    pub(crate) const fn with_requested_target(mut self, requested_target: Duration) -> Self {
        self.requested_target = requested_target;
        self
    }

    #[cfg(test)]
    pub(crate) const fn phase(self) -> HlsManifestSeekDiagnosticPhase {
        self.phase
    }
}

impl Display for HlsManifestSegmentSeekMarker {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let segment = self.anchor.manifest_segment;
        write!(
            formatter,
            "kind=hls_manifest_segment_seek phase={} component_role={} manifest_selection_id={} landing_policy={} source_generation={} requested_target_ms={} actual_anchor_ms={} actual_decode_anchor_ms={} anchor_kind={} media_sequence={} discontinuity_sequence={} manifest_segment_index={} epoch_index={} restart_segment_index={} segment_start_ms={} segment_end_ms={}",
            self.phase.diagnostic_name(),
            self.component_role.diagnostic_name(),
            self.manifest_selection_id.0,
            landing_policy_name(self.landing_policy),
            self.source_generation.value(),
            duration_milliseconds(self.requested_target),
            media_time_milliseconds(self.anchor.position),
            media_time_milliseconds(self.anchor.decode_position),
            anchor_kind_name(self.anchor.kind),
            segment.media_sequence,
            segment.discontinuity_sequence,
            segment.manifest_segment_index,
            segment.epoch_index,
            segment.restart_segment.segment_index,
            duration_milliseconds(segment.timeline_start),
            duration_milliseconds(segment.timeline_end),
        )
    }
}

const fn landing_policy_name(policy: HlsVodSeekLandingPolicy) -> &'static str {
    match policy {
        HlsVodSeekLandingPolicy::DecodeFromOrBeforeTarget => "decode_from_or_before_target",
        HlsVodSeekLandingPolicy::PreferPostTargetRap => "prefer_post_target_rap",
    }
}

const fn anchor_kind_name(kind: HlsSeekAnchorKind) -> &'static str {
    match kind {
        HlsSeekAnchorKind::VideoRandomAccessPoint => "video_random_access_point",
        HlsSeekAnchorKind::AudioPacket => "audio_packet",
    }
}

fn duration_milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn media_time_milliseconds(position: media_core::MediaTime) -> u64 {
    duration_milliseconds(position.as_duration())
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::thread::ThreadId;

    use super::*;
    use crate::plan::{HlsManifestSeekPoint, HlsSegmentRestartCoordinate};

    #[derive(Debug, PartialEq, Eq)]
    struct CapturedLogRecord {
        level: log::Level,
        target: String,
        message: String,
    }

    #[derive(Default)]
    struct CaptureState {
        owner_thread: Option<ThreadId>,
        records: Vec<CapturedLogRecord>,
    }

    struct HlsMarkerCaptureLogger {
        state: Mutex<CaptureState>,
    }

    impl log::Log for HlsMarkerCaptureLogger {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= log::Level::Info
                && metadata.target() == HLS_MANIFEST_SELECTION_LOG_TARGET
        }

        fn log(&self, record: &log::Record<'_>) {
            if !self.enabled(record.metadata()) {
                return;
            }
            let current_thread = std::thread::current().id();
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.owner_thread.as_ref() != Some(&current_thread) {
                return;
            }
            state.records.push(CapturedLogRecord {
                level: record.level(),
                target: record.target().to_owned(),
                message: record.args().to_string(),
            });
        }

        fn flush(&self) {}
    }

    static CAPTURE_LOGGER: HlsMarkerCaptureLogger = HlsMarkerCaptureLogger {
        state: Mutex::new(CaptureState {
            owner_thread: None,
            records: Vec::new(),
        }),
    };
    static CAPTURE_LOGGER_INSTALLATION: OnceLock<Result<(), ()>> = OnceLock::new();
    static CAPTURE_SERIALIZATION: Mutex<()> = Mutex::new(());

    struct MarkerCaptureSession {
        _serialization: MutexGuard<'static, ()>,
        owner_thread: ThreadId,
    }

    impl MarkerCaptureSession {
        fn start() -> Self {
            let serialization = CAPTURE_SERIALIZATION
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let installation = CAPTURE_LOGGER_INSTALLATION.get_or_init(|| {
                log::set_logger(&CAPTURE_LOGGER)
                    .map(|()| log::set_max_level(log::LevelFilter::Info))
                    .map_err(|_| ())
            });
            assert!(
                installation.is_ok(),
                "global logger уже занят другим unit-test owner-ом"
            );
            log::set_max_level(log::LevelFilter::Info);
            let owner_thread = std::thread::current().id();
            let mut state = CAPTURE_LOGGER
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.records.clear();
            state.owner_thread = Some(owner_thread);
            drop(state);
            Self {
                _serialization: serialization,
                owner_thread,
            }
        }

        fn finish(self) -> Vec<CapturedLogRecord> {
            let mut state = CAPTURE_LOGGER
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.owner_thread = None;
            std::mem::take(&mut state.records)
        }
    }

    impl Drop for MarkerCaptureSession {
        fn drop(&mut self) {
            let mut state = CAPTURE_LOGGER
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.owner_thread.as_ref() == Some(&self.owner_thread) {
                state.owner_thread = None;
                state.records.clear();
            }
        }
    }

    fn marker(phase: HlsManifestSeekDiagnosticPhase) -> HlsManifestSegmentSeekMarker {
        HlsManifestSegmentSeekMarker::new(
            phase,
            HlsManifestComponentRole::Video,
            HlsVodSeekLandingPolicy::PreferPostTargetRap,
            SourceGeneration::new(17),
            Duration::from_millis(12_345),
            HlsSeekAnchor {
                epoch_index: 2,
                restart_segment: HlsSegmentRestartCoordinate { segment_index: 3 },
                manifest_segment: HlsManifestSeekPoint {
                    media_sequence: 91,
                    discontinuity_sequence: 7,
                    manifest_segment_index: 5,
                    epoch_index: 2,
                    restart_segment: HlsSegmentRestartCoordinate { segment_index: 3 },
                    timeline_start: Duration::from_millis(12_000),
                    timeline_end: Duration::from_millis(18_000),
                },
                timeline_origin: Duration::from_millis(12_000),
                epoch_timestamp_origin: Duration::from_millis(900),
                position: media_core::MediaTime::from_millis(12_600),
                decode_position: media_core::MediaTime::from_millis(12_480),
                kind: HlsSeekAnchorKind::VideoRandomAccessPoint,
            },
        )
    }

    #[test]
    fn committed_marker_formats_typed_manifest_identity_without_locator_material() {
        let initial_open = marker(HlsManifestSeekDiagnosticPhase::InitialOpen).to_string();
        let initial_restore = marker(HlsManifestSeekDiagnosticPhase::InitialRestore).to_string();
        let preview = marker(HlsManifestSeekDiagnosticPhase::Preview).to_string();
        let rendered = marker(HlsManifestSeekDiagnosticPhase::FinalReceipt).to_string();
        assert!(initial_open.contains("phase=initial_open"));
        assert!(initial_restore.contains("phase=initial_restore"));
        assert!(preview.contains("phase=preview"));
        assert!(rendered.contains("kind=hls_manifest_segment_seek"));
        assert!(rendered.contains("phase=final_receipt"));
        assert!(rendered.contains("component_role=video"));
        assert!(rendered.contains("source_generation=17"));
        assert!(rendered.contains("media_sequence=91"));
        assert!(rendered.contains("discontinuity_sequence=7"));
        assert!(rendered.contains("segment_start_ms=12000"));
        assert!(rendered.contains("segment_end_ms=18000"));
        assert!(rendered.contains("actual_anchor_ms=12600"));
        assert!(!rendered.contains("actual_rap_ms"));
        assert!(rendered.contains("manifest_selection_id="));
        for forbidden in [
            "https://",
            "?token=",
            "authorization",
            "cookie",
            "segment.ts",
        ] {
            assert!(!rendered.to_ascii_lowercase().contains(forbidden));
        }
        assert_eq!(
            marker(HlsManifestSeekDiagnosticPhase::Preview).phase(),
            HlsManifestSeekDiagnosticPhase::Preview
        );
    }

    #[test]
    fn committed_marker_is_visible_at_info_through_neutral_log_facade() {
        let marker = marker(HlsManifestSeekDiagnosticPhase::FinalReceipt);
        let expected_message = marker.to_string();
        let capture = MarkerCaptureSession::start();

        marker.emit();

        let records = capture.finish();
        assert_eq!(records.len(), 1);
        let captured = &records[0];
        assert_eq!(captured.level, log::Level::Info);
        assert_eq!(captured.target, HLS_MANIFEST_SELECTION_LOG_TARGET);
        assert_eq!(captured.message, expected_message);
        let normalized = captured.message.to_ascii_lowercase();
        for forbidden in [
            "https://",
            "uri=",
            "path=",
            "query",
            "token",
            "authorization",
            "header",
            "cookie",
            "key=",
            "map=",
            "hash",
            "resource_id",
            "request_id",
        ] {
            assert!(
                !normalized.contains(forbidden),
                "утечка locator material: {forbidden}"
            );
        }
    }
}
