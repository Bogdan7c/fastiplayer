//! Поднятие окна по запросу второго запуска.
//!
//! Единственный winit-aware файл модуля `instance_forwarding`. Политика:
//! - есть билет активации → предъявить его композитору (патч winit
//!   `external_activation`; на Wayland это единственный способ поднять окно,
//!   на X11 — штатный `_NET_ACTIVE_WINDOW`);
//! - билета нет (второй запуск из терминала) или композитор не поддерживает
//!   активацию → попросить внимания: значок в панели задач подсветится, а на X11
//!   дополнительно сработает `focus_window`.
//!
//! Композитор вправе отказать (защита от кражи фокуса), об этом приложение не
//! узнаёт; поэтому результат — «что попросили», а не «что получилось».

use desktop_integration::WindowActivationToken;
use winit::window::{UserAttentionType, Window};

/// Каким способом попросили поднять окно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowRaiseRequest {
    /// Композитору предъявлен билет активации второго запуска.
    ActivatedWithToken,
    /// Билета нет или он не поддержан: окно попросило внимания пользователя.
    AttentionRequested,
}

/// Просит рабочий стол вывести окно наверх.
pub(crate) fn raise_window(
    window: &Window,
    activation_token: Option<WindowActivationToken>,
) -> WindowRaiseRequest {
    // Свёрнутое окно сначала разворачивается: иначе активация ничего не покажет.
    window.set_minimized(false);
    if let Some(activation_token) = activation_token
        && activate_with_token(window, activation_token)
    {
        return WindowRaiseRequest::ActivatedWithToken;
    }
    window.focus_window();
    window.request_user_attention(Some(UserAttentionType::Informational));
    WindowRaiseRequest::AttentionRequested
}

/// `true` — билет передан композитору.
#[cfg(target_os = "linux")]
fn activate_with_token(window: &Window, activation_token: WindowActivationToken) -> bool {
    use winit::platform::external_activation::WindowExtExternalActivation;
    use winit::window::ActivationToken;

    match window.activate_with_token(ActivationToken::from_raw(activation_token.into_string())) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "Композитор не поддерживает активацию окна по билету");
            false
        }
    }
}

/// На других платформах пересылки пока нет; билет не используется.
#[cfg(not(target_os = "linux"))]
fn activate_with_token(_window: &Window, _activation_token: WindowActivationToken) -> bool {
    false
}
