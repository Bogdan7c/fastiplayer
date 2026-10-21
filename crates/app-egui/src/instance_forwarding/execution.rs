//! Исполнение одного пересланного запроса: порядок шагов без знания winit и `AppState`.
//!
//! Порядок закреплён здесь, а не в shell: поднять окно → поставить файлы → войти в
//! фуллскрин, если просили (`--fullscreen`, cpu-parity 06b). Файлы ставятся раньше режима
//! окна, чтобы фильм открывался уже в новом режиме тем же путём, что кнопка Open, а
//! фуллскрин — тем же путём, что кнопка фуллскрина ([`crate::window_fullscreen`]).
//! Сами действия выполняет [`ForwardedRequestTarget`]: в приложении — склейка shell-а,
//! в тестах — запись шагов.

use desktop_integration::{
    ForwardedInstanceRequest, ForwardedInstanceRequestParts, FullscreenIntent,
    WindowActivationToken,
};

use super::external_open_request_for;
use crate::external_open::request::ExternalOpenRequest;

/// Что умеет исполнитель пересланного запроса.
pub(crate) trait ForwardedRequestTarget {
    /// Вывести окно наверх (билетом активации, если он есть).
    fn raise_window(&mut self, activation_token: Option<WindowActivationToken>);
    /// Открыть файлы/ссылки как бросок на видео (правила сессии 12).
    fn open_forwarded_media(&mut self, request: ExternalOpenRequest);
    /// Перевести окно в фуллскрин; уже полноэкранное остаётся как есть.
    fn enter_fullscreen(&mut self);
}

/// Исполняет запрос по порядку: окно → файлы → полноэкранный режим.
pub(crate) fn execute_forwarded_request(
    request: ForwardedInstanceRequest,
    target: &mut impl ForwardedRequestTarget,
) {
    let ForwardedInstanceRequestParts {
        action,
        activation_token,
        fullscreen,
    } = request.into_parts();
    target.raise_window(activation_token);
    if let Some(open_request) = external_open_request_for(action) {
        target.open_forwarded_media(open_request);
    }
    match fullscreen {
        FullscreenIntent::Enter => target.enter_fullscreen(),
        FullscreenIntent::LeaveAsIs => {}
    }
}
