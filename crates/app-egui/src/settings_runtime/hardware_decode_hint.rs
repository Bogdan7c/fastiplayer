//! Предупреждение в окне настроек: выбран режим «Hardware», а VA-API недоступен.
//!
//! Владелец решения (UX edge cases 2, B6): режим «Hardware» не имеет запасного пути
//! (`RequireBackend(vaapi)`), поэтому без VA-API ни одно видео не откроется. Пользователь
//! должен узнать об этом ДО открытия файла, одной строкой рядом с самой настройкой.
//!
//! Источник истины — capability report, который приложение уже собрало при старте
//! (`probe_system_capabilities`). Новых probe здесь нет и не должно быть: окно настроек
//! рисуется на UI-потоке, а probe обращается к драйверу. Пока отчёта нет, состояние
//! [`HardwareDecodeAvailability::Unknown`] и строка не показывается: не врём.

use capability_core::SystemCapabilities;
use codec_core::DecodeBackendId;
use fastiplayer_config::VideoBackendPreference;

/// Стабильный id настройки режима декодирования (см. `VideoConfig::preferred_backend`).
pub(super) const VIDEO_BACKEND_SETTING_ID: &str = "video.preferred_backend";

/// Текст предупреждения; названия режимов совпадают с подписями в окне настроек.
pub(super) const HARDWARE_UNAVAILABLE_WARNING: &str = "На этом компьютере аппаратное \
    декодирование (VA-API) недоступно — видео не откроется. Выберите режим «Авто».";

/// Что известно о VA-API на этой машине.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum HardwareDecodeAvailability {
    /// Capability report ещё не получен (или VA-API в нём не упомянут) — ничего не утверждаем.
    #[default]
    Unknown,

    /// VA-API прошёл probe и умеет хотя бы один формат, пригодный для показа.
    Available,

    /// VA-API не прошёл probe либо не даёт ни одного пригодного для показа формата.
    Unavailable,
}

impl HardwareDecodeAvailability {
    /// Выводит доступность VA-API из уже готового capability report-а.
    ///
    /// Поддержку отдельных кодеков намеренно не разбираем: «недоступно» говорим только
    /// когда VA-API не годится вообще (проблема кодека конкретного файла — это
    /// отдельная ошибка открытия).
    pub(crate) fn from_system_capabilities(capabilities: &SystemCapabilities) -> Self {
        let vaapi_id = DecodeBackendId::vaapi();
        let Some(vaapi) = capabilities
            .video_backends
            .iter()
            .find(|backend| backend.backend_id == vaapi_id)
        else {
            return Self::Unknown;
        };
        let has_playable_output = capabilities
            .supported_video_outputs()
            .any(|output| output.backend == vaapi_id);
        if vaapi.status.is_available() && has_playable_output {
            Self::Available
        } else {
            Self::Unavailable
        }
    }
}

/// Строка предупреждения для поля режима декодирования, если она нужна.
///
/// Показывается только для выбранного (в черновике) «Hardware» и только при твёрдом
/// знании, что VA-API недоступен.
pub(super) fn hardware_unavailable_warning(
    draft_preference: VideoBackendPreference,
    availability: HardwareDecodeAvailability,
) -> Option<&'static str> {
    match (draft_preference, availability) {
        (VideoBackendPreference::Hardware, HardwareDecodeAvailability::Unavailable) => {
            Some(HARDWARE_UNAVAILABLE_WARNING)
        }
        _ => None,
    }
}
