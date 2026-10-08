//! Наблюдения кадра для автоскрытия: был ли ввод и что держит chrome видимым.

use player_core::PlaybackState;

use super::{ChromeHoldReason, ChromeHolds};

/// Был ли в этом кадре пользовательский ввод, который должен показать chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UserActivity {
    /// Ввода не было.
    Idle,
    /// Мышь двигалась, нажата кнопка/клавиша, прокручено колесо или касание экрана.
    Detected,
}

impl UserActivity {
    /// Определяет активность по сырому вводу egui до отрисовки кадра.
    ///
    /// `MouseMoved` (сырое смещение устройства) не учитывается: оно может приходить без
    /// движения курсора над окном. Отпускание клавиши тоже не в счёт — показывает нажатие.
    /// `PointerGone` (курсор ушёл из окна) не активность: ушедший курсор не должен
    /// возвращать панели.
    #[must_use]
    pub(crate) fn from_raw_input(raw_input: &egui::RawInput) -> Self {
        let has_activity = raw_input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::PointerMoved(_)
                    | egui::Event::PointerButton { .. }
                    | egui::Event::MouseWheel { .. }
                    | egui::Event::Touch { .. }
                    | egui::Event::Text(_)
                    | egui::Event::Key { pressed: true, .. }
            )
        });
        if has_activity {
            Self::Detected
        } else {
            Self::Idle
        }
    }
}

/// Факты кадра, известные только после отрисовки панелей.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChromeHoldObservation<'a> {
    /// Видимые прямоугольники заголовка и нижней панели в этом кадре.
    pub(crate) visible_chrome_rects: &'a [egui::Rect],
    /// Открыт ли (или открывается) сайдбар.
    pub(crate) sidebar_open: bool,
    /// Состояние воспроизведения из snapshot-а кадра.
    pub(crate) playback_state: PlaybackState,
}

impl ChromeHoldObservation<'_> {
    /// Собирает причины удержания из egui-контекста после отрисовки виджетов.
    #[must_use]
    pub(crate) fn observe(&self, ctx: &egui::Context) -> ChromeHolds {
        let mut holds = ChromeHolds::default();
        let pointer_position = ctx.input(|input| input.pointer.hover_pos());
        if pointer_position.is_some_and(|position| {
            self.visible_chrome_rects
                .iter()
                .any(|chrome_rect| chrome_rect.contains(position))
        }) {
            holds = holds.with(ChromeHoldReason::PointerOverChrome);
        }
        if self.sidebar_open {
            holds = holds.with(ChromeHoldReason::SidebarOpen);
        }
        if egui::Popup::is_any_open(ctx) {
            holds = holds.with(ChromeHoldReason::PopupOpen);
        }
        if ctx.dragged_id().is_some() {
            holds = holds.with(ChromeHoldReason::PointerDrag);
        }
        if !playback_keeps_running(self.playback_state) {
            holds = holds.with(ChromeHoldReason::PlaybackNotRunning);
        }
        holds
    }
}

/// Идёт ли воспроизведение с точки зрения автоскрытия.
///
/// Буферизация, seek, scrub и доигрывание хвоста считаются «идёт»: это короткие фазы
/// внутри просмотра, и панели не должны выскакивать на каждой подгрузке сети. Пауза,
/// остановка, конец, ошибка, открытие и пустой плеер держат панели видимыми — так делают
/// привычные плееры (решение по умолчанию сессии UX 14). Match без `_`: новый вариант
/// состояния заставит явно решить его судьбу.
fn playback_keeps_running(playback_state: PlaybackState) -> bool {
    match playback_state {
        PlaybackState::Playing
        | PlaybackState::Buffering
        | PlaybackState::Seeking
        | PlaybackState::Scrubbing
        | PlaybackState::Draining => true,
        PlaybackState::Idle
        | PlaybackState::Opening
        | PlaybackState::Paused
        | PlaybackState::Ended
        | PlaybackState::Stopped
        | PlaybackState::Failed => false,
    }
}
