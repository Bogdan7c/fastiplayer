use super::config::VideoBackendStartupRequest;
use super::{StartedVideoBackend, vaapi};

/// Production selector, который скрывает текущий decoder backend от factory/session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProductionVideoBackendSelector {
    /// Выбранный internal adapter slot для production startup.
    selected_backend: SelectedProductionVideoBackend,
}

impl ProductionVideoBackendSelector {
    /// Создаёт selector с текущей production политикой выбора.
    #[must_use]
    pub(super) const fn production_default() -> Self {
        Self {
            selected_backend: SelectedProductionVideoBackend::current(),
        }
    }

    /// Возвращает выбранный backend без запуска decoder thread-а.
    #[must_use]
    pub(super) const fn select_backend(&self) -> SelectedProductionVideoBackend {
        self.selected_backend
    }

    /// Запускает выбранный backend и возвращает только neutral started wrapper.
    pub(super) fn start_video_backend(
        &self,
        startup_request: &VideoBackendStartupRequest<'_>,
    ) -> anyhow::Result<StartedVideoBackend> {
        self.select_backend().start_video_backend(startup_request)
    }
}

impl Default for ProductionVideoBackendSelector {
    /// Сохраняет текущую production policy для call sites без явного selector-а.
    fn default() -> Self {
        Self::production_default()
    }
}

/// Internal selected backend handle; наружу не раскрывает concrete implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SelectedProductionVideoBackend {
    /// Adapter slot, который будет выполнять concrete startup.
    adapter: ProductionVideoBackendAdapter,
}

impl SelectedProductionVideoBackend {
    /// Возвращает текущий production adapter slot.
    const fn current() -> Self {
        Self {
            adapter: ProductionVideoBackendAdapter::CurrentHardwareDecoder,
        }
    }

    /// Делегирует startup adapter-у и упаковывает decoder thread в neutral wrapper.
    fn start_video_backend(
        self,
        startup_request: &VideoBackendStartupRequest<'_>,
    ) -> anyhow::Result<StartedVideoBackend> {
        match self.adapter {
            ProductionVideoBackendAdapter::CurrentHardwareDecoder => {
                let decoder_thread = vaapi::start_video_decoder_thread(startup_request)?;

                Ok(StartedVideoBackend::from_decoder_thread(decoder_thread))
            }
        }
    }
}

/// Закрытый список production adapter slots; второй backend добавится здесь без worker/session changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProductionVideoBackendAdapter {
    /// Единственный текущий production hardware decoder slot.
    CurrentHardwareDecoder,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что selection отделён от startup и не требует GPU/backend resources.
    #[test]
    fn selector_exposes_current_production_selection_without_startup() {
        let selector = ProductionVideoBackendSelector::production_default();

        assert_eq!(
            selector.select_backend(),
            SelectedProductionVideoBackend::current()
        );
    }
}
