//! Как применять изменение renderer-bound настроек: на месте или полным пересозданием.
//!
//! Решение владельца сессии 02a (`user/gles2-renderer/`): изменение ТОЛЬКО `[render.vulkan]`
//! (present mode, задержка кадра) применяется перенастройкой surface текущего рендера —
//! без нового device, без пересоздания импорта кадров и без второй surface на окне. Всё
//! остальное (`render.tone_mapping`, будущие случаи)
//! — полным пересозданием, которое соблюдает «одно окно — одна surface» через
//! «отпустить окно» старым рендером.

use fastiplayer_settings::RenderCommittedSettingsUpdate;

/// Каким путём применить разницу между действующими и новыми committed настройками рендера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RendererSettingsChange {
    /// Ничего renderer-bound не изменилось: применять нечего (no-op).
    Unchanged,

    /// Изменились только настройки показа (`[render.vulkan]`): перенастройка на месте.
    PresentationOnly,

    /// Изменилось что-то, кроме настроек показа: полное пересоздание рендера.
    FullRecreation,
}

/// Классифицирует изменение настроек рендера (чистая функция, без доступа к рендеру).
pub(crate) fn classify_renderer_settings_change(
    previous: &RenderCommittedSettingsUpdate,
    next: &RenderCommittedSettingsUpdate,
) -> RendererSettingsChange {
    // Деструктуризация без `..`: новое поле в payload-е не скомпилируется, пока его не
    // отнесут к «показу» или к «пересозданию» здесь.
    let RenderCommittedSettingsUpdate {
        tone_mapping,
        vulkan,
    } = next;
    // `render.profile` сюда не попадает: смена профиля действует только после перезапуска
    // приложения и живой рендер не пересоздаёт (решение владельца, сессия 09, №37).
    let device_bound_changed = *tone_mapping != previous.tone_mapping;
    if device_bound_changed {
        return RendererSettingsChange::FullRecreation;
    }
    if *vulkan != previous.vulkan {
        return RendererSettingsChange::PresentationOnly;
    }
    RendererSettingsChange::Unchanged
}

#[cfg(test)]
mod tests {
    use fastiplayer_config::{AppConfig, ToneMappingMode, VulkanPresentMode};
    use fastiplayer_settings::RenderCommittedSettingsUpdate;

    use super::{RendererSettingsChange, classify_renderer_settings_change};

    fn committed_defaults() -> RenderCommittedSettingsUpdate {
        let config = AppConfig::default();
        RenderCommittedSettingsUpdate {
            tone_mapping: config.render.tone_mapping,
            vulkan: config.render.vulkan,
        }
    }

    /// Таблица: какая правка настроек каким путём применяется.
    #[test]
    fn only_vulkan_presentation_changes_are_applied_in_place() {
        let defaults = committed_defaults();
        let different_tone_mapping = if defaults.tone_mapping == ToneMappingMode::Auto {
            ToneMappingMode::Disabled
        } else {
            ToneMappingMode::Auto
        };
        let different_present_mode = if defaults.vulkan.present_mode == VulkanPresentMode::Mailbox {
            VulkanPresentMode::Fifo
        } else {
            VulkanPresentMode::Mailbox
        };

        type Edit = fn(&mut RenderCommittedSettingsUpdate, &Variations);
        let cases: [(&str, Edit, RendererSettingsChange); 6] = [
            ("nothing", |_, _| {}, RendererSettingsChange::Unchanged),
            (
                "max frame latency",
                |next, _| next.vulkan.max_frame_latency += 1,
                RendererSettingsChange::PresentationOnly,
            ),
            (
                "present mode",
                |next, variations| next.vulkan.present_mode = variations.present_mode,
                RendererSettingsChange::PresentationOnly,
            ),
            (
                "present mode and latency together",
                |next, variations| {
                    next.vulkan.present_mode = variations.present_mode;
                    next.vulkan.max_frame_latency += 1;
                },
                RendererSettingsChange::PresentationOnly,
            ),
            (
                "tone mapping stub",
                |next, variations| next.tone_mapping = variations.tone_mapping,
                RendererSettingsChange::FullRecreation,
            ),
            (
                "tone mapping together with vulkan",
                |next, variations| {
                    next.tone_mapping = variations.tone_mapping;
                    next.vulkan.max_frame_latency += 1;
                },
                RendererSettingsChange::FullRecreation,
            ),
        ];
        let variations = Variations {
            tone_mapping: different_tone_mapping,
            present_mode: different_present_mode,
        };

        for (name, edit, expected) in cases {
            let mut next = defaults.clone();
            edit(&mut next, &variations);

            assert_eq!(
                classify_renderer_settings_change(&defaults, &next),
                expected,
                "case '{name}'"
            );
        }
    }

    /// Значения, отличные от умолчаний, для строк таблицы.
    struct Variations {
        tone_mapping: ToneMappingMode,
        present_mode: VulkanPresentMode,
    }
}
