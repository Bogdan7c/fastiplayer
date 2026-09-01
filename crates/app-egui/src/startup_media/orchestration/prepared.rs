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

/// Prepared topology — единственный app-owned источник authoritative consumer proof-а.
pub(super) fn prepared_startup_consumer_proof(
    tracks: &[media_core::TrackInfo],
) -> crate::startup_readiness::StartupPreparedConsumerProof {
    let audio = if tracks
        .iter()
        .any(|track| track.kind == media_core::TrackKind::Audio)
    {
        crate::startup_readiness::StartupAudioProof::Required
    } else {
        crate::startup_readiness::StartupAudioProof::NotPresent
    };
    let video = if tracks
        .iter()
        .any(|track| track.kind == media_core::TrackKind::Video)
    {
        crate::startup_readiness::StartupVideoProof::Required
    } else {
        crate::startup_readiness::StartupVideoProof::NotPresent
    };
    crate::startup_readiness::StartupPreparedConsumerProof { audio, video }
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
    /// Native HDS сохраняет VOD window/recovery до app composition.
    NativeHds {
        source: crate::media_open::NativeHdsUrl,
        prepared: Box<super::super::native_hds::PreparedNativeHdsMedia>,
    },
    /// Native Smooth сохраняет VOD recovery attachment до app composition.
    NativeSmooth {
        source: crate::media_open::NativeSmoothUrl,
        prepared: Box<super::super::native_smooth::PreparedNativeSmoothMedia>,
    },
}
