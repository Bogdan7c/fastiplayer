//! Composition mapping committed app config в нейтральный media-install constraint.

use player_core::MediaInstallVideoBackendConstraint;
use rustiplayer_config::VideoBackendPreference;

/// Захватывает backend policy одного staged media install request-а.
///
/// `Auto` оставляет exact выбор player-у. Явные preferences запрещают fallback
/// на другой backend ещё до запроса app-owned decoder resources.
pub(crate) fn media_install_video_backend_constraint(
    preference: VideoBackendPreference,
) -> MediaInstallVideoBackendConstraint {
    match preference {
        VideoBackendPreference::Auto => MediaInstallVideoBackendConstraint::AnyPlayable,
        VideoBackendPreference::Hardware => {
            MediaInstallVideoBackendConstraint::RequireBackend(codec_core::DecodeBackendId::vaapi())
        }
        VideoBackendPreference::Software => MediaInstallVideoBackendConstraint::RequireBackend(
            video_ffmpeg::ffmpeg_software_backend_id(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Все config variants обязаны иметь явный и exhaustive neutral mapping.
    #[test]
    fn committed_preferences_map_to_request_scoped_backend_constraints() {
        assert_eq!(
            media_install_video_backend_constraint(VideoBackendPreference::Auto),
            MediaInstallVideoBackendConstraint::AnyPlayable
        );
        assert_eq!(
            media_install_video_backend_constraint(VideoBackendPreference::Hardware),
            MediaInstallVideoBackendConstraint::RequireBackend(codec_core::DecodeBackendId::vaapi())
        );
        assert_eq!(
            media_install_video_backend_constraint(VideoBackendPreference::Software),
            MediaInstallVideoBackendConstraint::RequireBackend(
                video_ffmpeg::ffmpeg_software_backend_id()
            )
        );
    }
}
