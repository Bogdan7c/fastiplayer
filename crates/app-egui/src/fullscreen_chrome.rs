//! Автоскрытие «chrome» (заголовка, нижней панели и курсора) в полноэкранном режиме.
//!
//! Единственный владелец решения «показывать ли сейчас панели и курсор». Модуль не рисует
//! панели и не знает, как они устроены: он отдаёт [`ChromePresentation`] (насколько панели
//! уехали за край экрана и виден ли курсор), а сдвигом занимается [`crate::ui::edge_slide`].
//!
//! Решения владельца (сессия UX 14, 8 октября 2026):
//! - прятать только в полноэкранном режиме; в обычном окне всё всегда видно;
//! - задержка настраивается (`ui.window.fullscreen_autohide_delay_ms`, `0` — не прятать);
//! - панели уезжают с той же скоростью и кривой, что и сайдбар, и так же уважают
//!   «уменьшенную анимацию» (там длительность равна нулю — переход мгновенный).
//!
//! Цикл кадра:
//! 1. [`FullscreenChromeController::advance`] — до отрисовки UI: ввод этого кадра + причины
//!    удержания, замеченные в конце прошлого кадра, решают, прятать ли панели;
//! 2. UI рисует панели по [`FullscreenChromeController::presentation`];
//! 3. [`FullscreenChromeController::finish_frame`] — внутри egui-прохода после отрисовки:
//!    запоминает причины удержания (курсор над панелью, открыт попап и т.д.) и прячет курсор.

mod observation;

use std::time::{Duration, Instant};

use animation_core::{Easing, SlideTransition};

pub(crate) use observation::{ChromeHoldObservation, UserActivity};

/// Максимальный шаг анимации за кадр: после долгой паузы без перерисовки панели не
/// «телепортируются», а продолжают плавное движение (тот же приём, что у сайдбара).
const MAX_CHROME_SLIDE_FRAME_DT_SECONDS: f32 = 0.1;

/// Режим окна, от которого зависит, можно ли вообще прятать chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowPresentationMode {
    /// Обычное окно: панели и курсор всегда видны.
    Windowed,
    /// Полноэкранный режим: при бездействии chrome прячется.
    Fullscreen,
}

/// Задержка автоскрытия из подтверждённой конфигурации.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutohideDelay {
    /// Автоскрытие выключено (`0` в настройках).
    Disabled,
    /// Прятать после указанного времени без ввода.
    After(Duration),
}

impl AutohideDelay {
    /// Переводит значение настройки в миллисекундах; `0` означает «не прятать».
    #[must_use]
    pub(crate) fn from_config_millis(delay_ms: u16) -> Self {
        if delay_ms == 0 {
            Self::Disabled
        } else {
            Self::After(Duration::from_millis(u64::from(delay_ms)))
        }
    }
}

/// Причина держать chrome видимым, даже если пользователь ничего не трогает.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChromeHoldReason {
    /// Курсор над заголовком или нижней панелью: пользователь целится в кнопку.
    PointerOverChrome,
    /// Открыт сайдбар (плейлист, настройки, URL, информация).
    SidebarOpen,
    /// Открыто выпадающее меню или другой попап egui.
    PopupOpen,
    /// Идёт перетаскивание (таймлайн, громкость, строки плейлиста).
    PointerDrag,
    /// Видео не воспроизводится (пауза, остановка, ошибка, ничего не открыто).
    PlaybackNotRunning,
}

impl ChromeHoldReason {
    /// Бит причины в [`ChromeHolds`].
    const fn bit(self) -> u8 {
        match self {
            Self::PointerOverChrome => 1 << 0,
            Self::SidebarOpen => 1 << 1,
            Self::PopupOpen => 1 << 2,
            Self::PointerDrag => 1 << 3,
            Self::PlaybackNotRunning => 1 << 4,
        }
    }
}

/// Набор причин удержания одного кадра. Битовая маска вместо `Vec`, чтобы
/// ежекадровая проверка не аллоцировала память.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChromeHolds {
    mask: u8,
}

impl ChromeHolds {
    /// Добавляет причину удержания.
    #[must_use]
    pub(crate) const fn with(self, reason: ChromeHoldReason) -> Self {
        Self {
            mask: self.mask | reason.bit(),
        }
    }

    /// Есть ли среди причин указанная.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn contains(self, reason: ChromeHoldReason) -> bool {
        self.mask & reason.bit() != 0
    }

    /// Нет ни одной причины держать chrome видимым.
    #[must_use]
    pub(crate) const fn is_empty(self) -> bool {
        self.mask == 0
    }
}

/// Должен ли быть виден курсор мыши.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorVisibility {
    Visible,
    Hidden,
}

/// Что рисовать в этом кадре.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ChromePresentation {
    /// Насколько панели уехали за край экрана: `0.0` — на месте, `1.0` — полностью скрыты.
    /// Уже со сглаживанием (та же кривая, что у сайдбара).
    pub(crate) hidden_fraction: f32,
    /// Виден ли курсор.
    pub(crate) cursor: CursorVisibility,
}

/// Ввод одного кадра для [`FullscreenChromeController::advance`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChromeFrameInput {
    pub(crate) mode: WindowPresentationMode,
    pub(crate) activity: UserActivity,
    pub(crate) delay: AutohideDelay,
    /// Длительность полного уезда/появления; `0` — мгновенно (reduced motion).
    pub(crate) slide_duration_seconds: f32,
    pub(crate) now: Instant,
}

/// Владелец видимости chrome в полноэкранном режиме.
#[derive(Debug)]
pub(crate) struct FullscreenChromeController {
    /// Позиция анимации: «открыто» = панели на месте. Тот же примитив, что у сайдбара.
    visibility: SlideTransition,
    /// Решено ли прятать chrome (цель анимации). Курсор прячется сразу по решению,
    /// не дожидаясь конца анимации панелей.
    hide_requested: bool,
    /// Момент последнего ввода или последнего кадра с причиной удержания.
    last_activity_at: Option<Instant>,
    /// Когда chrome спрячется, если ничего не произойдёт. Нужен event loop-у, чтобы
    /// проснуться без ввода (например, на остановленном видео без перерисовок).
    hide_deadline: Option<Instant>,
    /// Причины удержания, замеченные в конце прошлого кадра.
    holds: ChromeHolds,
    last_tick: Option<Instant>,
}

impl Default for FullscreenChromeController {
    fn default() -> Self {
        Self {
            visibility: SlideTransition::open(),
            hide_requested: false,
            last_activity_at: None,
            hide_deadline: None,
            holds: ChromeHolds::default(),
            last_tick: None,
        }
    }
}

impl FullscreenChromeController {
    /// Продвигает таймер бездействия и анимацию. Вызывается один раз за кадр до отрисовки UI.
    pub(crate) fn advance(&mut self, input: ChromeFrameInput) {
        let dt_seconds = self
            .last_tick
            .map(|last_tick| {
                input
                    .now
                    .saturating_duration_since(last_tick)
                    .as_secs_f32()
                    .min(MAX_CHROME_SLIDE_FRAME_DT_SECONDS)
            })
            .unwrap_or(0.0);
        self.last_tick = Some(input.now);

        if input.mode == WindowPresentationMode::Windowed {
            // В окне раскладка обязана совпадать с прежней сразу, без анимации возврата.
            // Таймер «взводится» заново: вход в фуллскрин всегда даёт полную задержку.
            self.visibility = SlideTransition::open();
            self.hide_requested = false;
            self.last_activity_at = Some(input.now);
            self.hide_deadline = None;
            return;
        }

        let is_held = !self.holds.is_empty();
        if input.activity == UserActivity::Detected || is_held || self.last_activity_at.is_none() {
            self.last_activity_at = Some(input.now);
        }
        let last_activity_at = self.last_activity_at.unwrap_or(input.now);

        self.hide_requested = match input.delay {
            AutohideDelay::Disabled => false,
            AutohideDelay::After(delay) => {
                input.now.saturating_duration_since(last_activity_at) >= delay
            }
        };
        // Пока chrome держит причина удержания, будильник не нужен: снять удержание может
        // только ввод или смена состояния, а они сами вызовут кадр. Иначе на паузе event
        // loop просыпался бы каждые N секунд впустую.
        self.hide_deadline = match input.delay {
            AutohideDelay::After(delay) if !self.hide_requested && !is_held => {
                last_activity_at.checked_add(delay)
            }
            AutohideDelay::After(_) | AutohideDelay::Disabled => None,
        };

        self.visibility.set_target_open(!self.hide_requested);
        self.visibility
            .advance(dt_seconds, input.slide_duration_seconds);
    }

    /// Запоминает причины удержания, замеченные после отрисовки кадра; учтутся в следующем
    /// [`Self::advance`].
    pub(crate) fn record_holds(&mut self, holds: ChromeHolds) {
        self.holds = holds;
    }

    /// Завершает кадр внутри egui-прохода: собирает причины удержания и прячет курсор.
    ///
    /// Курсор прячется через `CursorIcon::None`, который egui-winit превращает в
    /// `Window::set_cursor_visible(false)`. Вызывать после всех виджетов кадра, иначе
    /// hover-иконка виджета перезапишет решение.
    pub(crate) fn finish_frame(
        &mut self,
        ctx: &egui::Context,
        observation: ChromeHoldObservation<'_>,
    ) {
        self.record_holds(observation.observe(ctx));
        if self.presentation().cursor == CursorVisibility::Hidden {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
    }

    /// Состояние для отрисовки текущего кадра.
    #[must_use]
    pub(crate) fn presentation(&self) -> ChromePresentation {
        ChromePresentation {
            hidden_fraction: 1.0 - self.visibility.eased_progress(Easing::EaseInOutCubic),
            cursor: if self.hide_requested {
                CursorVisibility::Hidden
            } else {
                CursorVisibility::Visible
            },
        }
    }

    /// `true`, пока панели едут: нужен следующий кадр даже без воспроизведения.
    #[must_use]
    pub(crate) fn is_animating(&self) -> bool {
        self.visibility.is_animating()
    }

    /// Момент, когда chrome спрячется без ввода; `None` — будильник не нужен.
    #[must_use]
    pub(crate) fn next_wake_deadline(&self) -> Option<Instant> {
        self.hide_deadline
    }
}

#[cfg(test)]
mod tests;
