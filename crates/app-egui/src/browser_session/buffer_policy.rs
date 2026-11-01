//! «Текущая политика буфера» браузерного режима: окно буфера и потолок памяти для
//! СЛЕДУЮЩЕГО видео.
//!
//! Решение 96 и «зеркало браузера» (04d): ключи `[browser_session] buffer_ahead_s` и
//! `max_held_mb` из окна настроек действуют на следующее поколение (видео со страницы);
//! источник уже идущего потока не перестраивается — поток от страницы нельзя открыть
//! заново, а его потолок памяти и порог «полон» держатся на значении старта потока.
//!
//! Владелец значения — [`LiveBufferWindow`]. Пишет его главный поток (применение маршрута
//! `browser_session` в `SettingsRuntime` окна, через [`BrowserBufferWindowOwner`]), читает
//! поток приёма в момент создания нового потока ([`PushedStreamFactory`](super::translator::stream_port::PushedStreamFactory)),
//! поэтому значение лежит под замком и отдаётся копией: уже созданный поток своей копии
//! не меняет.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, PoisonError};

use fastiplayer_config::BrowserSessionBufferWindow;
use pushed_media_source::{BufferWindow, PushedSourcePolicy, PushedSourcePolicyError};
use tracing::warn;

use crate::settings_runtime::BrowserBufferWindowOwner;

/// Что пользователь выставил для следующего потока: окно вперёд и потолок памяти.
#[derive(Debug, Clone, Copy)]
struct NextStreamLimits {
    /// Окно буфера (запас вперёд).
    window: BufferWindow,
    /// Потолок памяти потока из config-а, байт.
    max_held_bytes: usize,
}

/// Общий владелец окна и потолка памяти для будущих потоков. Клонирование — ещё одна
/// ручка к тому же значению.
#[derive(Debug, Clone)]
pub(crate) struct LiveBufferWindow {
    /// Пределы, с которыми стартует следующий поток.
    next_stream: Arc<Mutex<NextStreamLimits>>,
    /// Тестовый потолок процесса (`held_bytes_override`): перекрывает потолок из config-а
    /// всегда, в том числе после смены настройки на лету.
    held_bytes_override: Option<NonZeroUsize>,
}

impl LiveBufferWindow {
    /// Начальные пределы — из config-а при старте процесса; `held_bytes_override` — тестовая
    /// переменная окружения (без неё `None`).
    pub(crate) fn new(
        initial: BrowserSessionBufferWindow,
        held_bytes_override: Option<NonZeroUsize>,
    ) -> Self {
        Self {
            next_stream: Arc::new(Mutex::new(next_stream_limits(initial))),
            held_bytes_override,
        }
    }

    /// Окно, с которым стартует следующий поток (копия: идущие потоки её не видят).
    #[cfg(test)]
    pub(crate) fn window_for_next_stream(&self) -> BufferWindow {
        self.limits().window
    }

    /// Политика следующего потока: `base` с окном и потолком памяти, выставленными
    /// пользователем к этому моменту (тестовый потолок процесса — поверх них).
    pub(crate) fn policy_for_next_stream(
        &self,
        base: PushedSourcePolicy,
    ) -> Result<PushedSourcePolicy, PushedSourcePolicyError> {
        let limits = self.limits();
        let policy = base.with_buffer_window(limits.window)?;
        let ceiling = self
            .held_bytes_override
            .or_else(|| NonZeroUsize::new(limits.max_held_bytes));
        match ceiling {
            Some(ceiling) => Ok(policy.with_max_held_stream_bytes(ceiling)),
            // Config прошёл проверку (потолок >= 64 МиБ); нулю взяться неоткуда, но молча
            // его не глотаем: остаётся потолок политики старта процесса.
            None => {
                warn!("The configured held bytes ceiling is zero; keeping the startup ceiling");
                Ok(policy)
            }
        }
    }

    /// Копия текущих пределов (замок живёт только внутри вызова).
    fn limits(&self) -> NextStreamLimits {
        *self
            .next_stream
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl BrowserBufferWindowOwner for LiveBufferWindow {
    fn adopt_buffer_window_for_next_stream(&self, window: BrowserSessionBufferWindow) {
        *self
            .next_stream
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = next_stream_limits(window);
    }
}

/// Перевод пределов из config-а в типы источника.
fn next_stream_limits(window: BrowserSessionBufferWindow) -> NextStreamLimits {
    NextStreamLimits {
        window: source_window(window),
        max_held_bytes: window.max_held_bytes(),
    }
}

/// Перевод окна из config-а в тип источника.
pub(crate) fn source_window(window: BrowserSessionBufferWindow) -> BufferWindow {
    BufferWindow {
        ahead: window.ahead(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fastiplayer_config::AppConfig;

    use super::*;

    const MIB: usize = 1024 * 1024;

    /// Окно из config-а с заданными запасом вперёд (с) и потолком памяти (МиБ).
    fn config_window(ahead_s: u16, max_held_mb: u16) -> BrowserSessionBufferWindow {
        let mut config = AppConfig::default();
        config.browser_session.buffer_ahead_s = ahead_s;
        config.browser_session.max_held_mb = max_held_mb;
        config.browser_session.buffer_window()
    }

    /// Порог «полон» политики — 75 % потолка: по нему видно, какой потолок в политике.
    fn full_at(policy: &PushedSourcePolicy) -> usize {
        policy.buffer_full_at_held_bytes()
    }

    fn base_policy() -> PushedSourcePolicy {
        PushedSourcePolicy::recommended().expect("policy")
    }

    /// Усыновление нового окна меняет то, с чем стартует следующий поток; ранее
    /// выданная копия (окно идущего потока) остаётся прежней.
    #[test]
    fn adopting_changes_only_the_window_of_the_next_stream() {
        let live = LiveBufferWindow::new(config_window(120, 320), None);
        let running_stream_window = live.window_for_next_stream();

        live.adopt_buffer_window_for_next_stream(config_window(45, 512));

        assert_eq!(running_stream_window.ahead, Duration::from_secs(120));
        assert_eq!(live.window_for_next_stream().ahead, Duration::from_secs(45));
    }

    /// Потолок из config-а доходит до политики потока (× МиБ); смена на лету меняет только
    /// политику, выданную ПОСЛЕ неё, — политика идущего потока остаётся со старым потолком.
    #[test]
    fn the_configured_ceiling_reaches_the_next_stream_policy_only() {
        let live = LiveBufferWindow::new(config_window(120, 200), None);
        let running = live.policy_for_next_stream(base_policy()).expect("policy");
        assert_eq!(full_at(&running), 200 * MIB / 100 * 75);

        live.adopt_buffer_window_for_next_stream(config_window(120, 1000));
        let next = live.policy_for_next_stream(base_policy()).expect("policy");

        assert_eq!(full_at(&running), 200 * MIB / 100 * 75, "running stream");
        assert_eq!(full_at(&next), 1000 * MIB / 100 * 75);
    }

    /// Тестовый потолок процесса перекрывает config, в том числе после смены настройки.
    #[test]
    fn the_test_override_beats_the_configured_ceiling() {
        let tiny = NonZeroUsize::new(4 * MIB).expect("non-zero");
        let live = LiveBufferWindow::new(config_window(120, 320), Some(tiny));
        let expected = full_at(&base_policy().with_max_held_stream_bytes(tiny));

        let before = live.policy_for_next_stream(base_policy()).expect("policy");
        live.adopt_buffer_window_for_next_stream(config_window(120, 2048));
        let after = live.policy_for_next_stream(base_policy()).expect("policy");

        assert_eq!(full_at(&before), expected);
        assert_eq!(full_at(&after), expected);
    }

    /// Ручки делят одно значение: усыновил один владелец — видит читатель на другом потоке.
    #[test]
    fn clones_share_the_value_across_threads() {
        let live = LiveBufferWindow::new(config_window(120, 320), None);
        let writer = live.clone();

        std::thread::spawn(move || {
            writer.adopt_buffer_window_for_next_stream(config_window(60, 400));
        })
        .join()
        .expect("writer thread");

        assert_eq!(live.window_for_next_stream().ahead, Duration::from_secs(60));
        let policy = live.policy_for_next_stream(base_policy()).expect("policy");
        assert_eq!(full_at(&policy), 400 * MIB / 100 * 75);
    }
}
