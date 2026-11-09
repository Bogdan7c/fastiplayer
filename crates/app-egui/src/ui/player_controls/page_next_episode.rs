//! Кнопка Next в окне браузерного режима: «следующая серия» страницы (сессия 14a, решение 283).
//!
//! Навигации по очереди в этом окне нет (`WindowViewProfile::offers_queue_navigation`):
//! Previous, Shuffle и Repeat не рисуются. Next возвращается на своё обычное место, только
//! когда страница сказала «следующая серия есть» и на экране нет рекламы — это решает
//! владелец состояния (`AppState::next_episode_offered`), сюда приходит готовое
//! [`PageNextEpisodeButton`].
//!
//! Клик публикует то же действие, что Next обычного окна (`TransportControlAction::Next`):
//! дальше его ведёт маршрут окна страницы (`AppState::route_page_led_transport_action` →
//! `activate_next`), общий с клавишей N и MPRIS Next — реклама главнее серии.

use egui::{Rect, Ui};

use super::ControlAction;
use super::transport;
use crate::playlist_runtime::NavigationControlAvailability;
use crate::ui::skin::ControlsStyle;

/// Есть ли в окне страницы кнопка Next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PageNextEpisodeButton {
    /// Страница предлагает следующую серию: кнопка на своём месте.
    Offered,
    /// Не предлагает (или реклама, или экстеншен старше 1.7): кнопки нет.
    #[default]
    NotOffered,
}

/// Рисует Next на его обычном месте, если следующая серия предложена.
pub(super) fn render(
    ui: &mut Ui,
    button_rect: Rect,
    button: PageNextEpisodeButton,
    controls_style: ControlsStyle,
    actions: &mut Vec<ControlAction>,
) {
    match button {
        PageNextEpisodeButton::Offered => transport::render_next_button(
            ui,
            button_rect,
            NavigationControlAvailability::Ready,
            controls_style,
            actions,
        ),
        PageNextEpisodeButton::NotOffered => {}
    }
}
