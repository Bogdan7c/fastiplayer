//! Активация уже открытого окна билетом другого процесса (additive-расширение
//! winit 0.30, добавлено Fastiplayer).
//!
//! Зачем это нужно. Когда пользователь открывает файл «Открыть с помощью» при уже
//! запущенном приложении, файловый менеджер выдаёт билет активации
//! (`XDG_ACTIVATION_TOKEN` на Wayland, `DESKTOP_STARTUP_ID` на X11) новому, второму
//! процессу. Второй процесс пересылает файл первому и завершается, а первый должен
//! вывести своё окно наверх. Штатный winit 0.30 умеет предъявлять билет только при
//! создании окна ([`WindowAttributesExtStartupNotify::with_activation_token`]), а
//! [`Window::focus_window`] на Wayland ничего не делает. Без билета композитор
//! (KWin, Mutter) по правилам защиты от кражи фокуса окно не поднимет.
//!
//! # Поведение по платформам
//!
//! - **Wayland:** билет предъявляется протоколу `xdg_activation_v1` для поверхности
//!   окна. Решение принимает композитор: просроченный или чужой билет он вправе
//!   проигнорировать, ошибки при этом не возникает.
//! - **X11:** билет не используется; вызывается штатный [`Window::focus_window`]
//!   (`_NET_ACTIVE_WINDOW`). Оконный менеджер тоже может отказать.
//!
//! Если композитор Wayland не поддерживает `xdg_activation_v1`, метод возвращает
//! [`NotSupportedError`], и вызывающий код может хотя бы попросить внимания
//! ([`Window::request_user_attention`]).
//!
//! [`WindowAttributesExtStartupNotify::with_activation_token`]:
//!     crate::platform::startup_notify::WindowAttributesExtStartupNotify::with_activation_token

use crate::error::NotSupportedError;
use crate::window::{ActivationToken, Window};

/// Расширение окна: вывести уже существующее окно наверх по билету активации.
pub trait WindowExtExternalActivation {
    /// Просит композитор активировать окно, предъявив билет другого процесса.
    ///
    /// Вызов неблокирующий: запрос уходит композитору вместе с ближайшей отправкой
    /// очереди Wayland. Успешный результат означает «запрос отправлен», а не
    /// «окно гарантированно поднято».
    fn activate_with_token(&self, token: ActivationToken) -> Result<(), NotSupportedError>;
}

impl WindowExtExternalActivation for Window {
    fn activate_with_token(&self, token: ActivationToken) -> Result<(), NotSupportedError> {
        let _span = tracing::debug_span!("winit::Window::activate_with_token").entered();
        // Как и остальные методы окна, запрос выполняется на главном потоке.
        self.window.maybe_wait_on_main(|window| window.activate_with_token(token))
    }
}
