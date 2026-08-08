//! Exact destination policy одного apply либо compensating rollback прохода.

use rustiplayer_config::{AppConfig, VideoBackendPreference};

/// Immutable cross-route policy, построенная до первой runtime owner mutation.
///
/// Поздний MediaService rebuild видит ту же backend policy, что и Player route,
/// а reverse rollback получает policy предыдущего committed document-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsRouteTargetPolicy {
    /// Exact destination существует у production settings transaction.
    Exact {
        /// Config-owned backend preference для всех media reinstall-ов этого прохода.
        video_backend_preference: VideoBackendPreference,
    },

    /// Generic no-owner route simulator не выполняет external media reinstall.
    ExternalOwnersUnavailable,
}

impl SettingsRouteTargetPolicy {
    /// Захватывает только policy, нужную cross-route active-media boundary.
    #[must_use]
    pub(crate) const fn from_config(config: &AppConfig) -> Self {
        Self::Exact {
            video_backend_preference: config.video.preferred_backend,
        }
    }

    /// Возвращает exact destination preference либо явно сообщает отсутствие owner-а.
    #[must_use]
    pub(crate) const fn video_backend_preference(self) -> Option<VideoBackendPreference> {
        match self {
            Self::Exact {
                video_backend_preference,
            } => Some(video_backend_preference),
            Self::ExternalOwnersUnavailable => None,
        }
    }
}
