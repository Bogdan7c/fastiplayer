//! Prepared startup ownership и общие доказательства до allocator/install gate-а.

use player_core::PlaybackIntent;

use crate::media_open::PreparedLocalOpenResult;
use crate::playlist_runtime::StartupRestoreTarget;

use super::super::PreparedYtDlpStartupMedia;

/// Применяет актуальную config policy к domain target до strong-open admission.
pub(crate) fn apply_restored_playback_policy(
    target: &mut StartupRestoreTarget,
    config: &rustiplayer_config::AppConfig,
) {
    target.set_playback_intent(PlaybackIntent::from_autoplay(!config.player.start_paused));
}

/// Prepared topology — единственный app-owned источник positive/absent audio proof-а.
pub(super) fn prepared_startup_audio_proof(
    tracks: &[media_core::TrackInfo],
) -> crate::startup_readiness::StartupAudioProof {
    if tracks
        .iter()
        .any(|track| track.kind == media_core::TrackKind::Audio)
    {
        crate::startup_readiness::StartupAudioProof::Required
    } else {
        crate::startup_readiness::StartupAudioProof::NotPresent
    }
}

/// Prepared ownership сохраняется до trusted allocator decision.
pub(crate) enum PreparedStartupMedia {
    /// Local file path уже открыт local owner-ом.
    Local(Box<PreparedLocalOpenResult>),
    /// Extractor result хранит stable service locator отдельно от temporary media URLs.
    Extractor {
        source_locator: service_ytdlp::YtDlpMediaLocator,
        prepared: Box<PreparedYtDlpStartupMedia>,
    },
    /// Direct progressive source уже скомпонован в neutral envelope.
    Direct {
        source_locator: service_direct_media::DirectMediaUrl,
        prepared_media: player_core::PreparedMedia,
        descriptor: Box<crate::media_open::PreparedWebMediaEnvelope>,
    },
    /// Native HLS сохраняет provider lifecycle до app composition.
    NativeHls {
        source: crate::media_open::NativeHlsUrl,
        prepared: Box<super::super::native_hls::PreparedNativeHlsMedia>,
    },
    /// Native static DASH сохраняет provider lifecycle до app composition.
    NativeDash {
        source: crate::media_open::NativeDashUrl,
        prepared: Box<super::super::native_dash::PreparedNativeDashMedia>,
    },
}
