//! Управление окном браузерного режима плеера так, как это делает рабочий стол: по MPRIS
//! на частной шине стенда (`busctl --address=…`).
//!
//! Окно занимает имя `org.mpris.MediaPlayer2.fastiplayer.browser` (сессия 04c) и понимает
//! `Play`, `Pause`, `Seek`, свойство `Position` и `Quit` корневого интерфейса (закрыть окно,
//! как закрыл бы человек; плеер при этом отправляет странице `return_to_page{T, paused}`).
//! Стенд меняет состояние плеера только этими публичными командами, внутрь плеера не лезет.
//!
//! Модуль владеет вызовами `busctl` и разбором его вывода; сценарии видят только намерения.

use std::process::Command;
use std::time::{Duration, Instant};

/// Имя окна браузерного режима на шине (зеркало `MPRIS_BROWSER_BUS_NAME` плеера).
const BROWSER_BUS_NAME: &str = "org.mpris.MediaPlayer2.fastiplayer.browser";
/// Объект MPRIS по спецификации.
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
/// Интерфейс `Player`.
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
/// Корневой интерфейс (`Quit`).
const ROOT_INTERFACE: &str = "org.mpris.MediaPlayer2";
/// Шаг опроса, пока окно не опубликовало управление.
const POLL_STEP: Duration = Duration::from_millis(200);

/// Ошибка управления плеером по MPRIS.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlayerControlError {
    /// `busctl` не запустился.
    #[error("cannot run busctl: {0}")]
    Spawn(String),
    /// `busctl` вернул ошибку (окна нет на шине, метод отказал).
    #[error("busctl {command} failed: {stderr}")]
    CommandFailed {
        /// Что вызывали.
        command: String,
        /// Текст ошибки.
        stderr: String,
    },
    /// Вывод `busctl` не похож на ожидаемый.
    #[error("unexpected busctl output for {property}: {output:?}")]
    UnexpectedOutput {
        /// Свойство, которое читали.
        property: &'static str,
        /// Вывод как есть.
        output: String,
    },
    /// Окно не стало управляемым за отведённое время.
    #[error("the browser window MPRIS did not become controllable within {0:?}")]
    NotControllable(Duration),
    /// Окно не закрылось (его имя осталось на шине) за отведённое время.
    #[error("the browser window MPRIS is still on the bus after {0:?}")]
    StillPresent(Duration),
}

/// Пульт окна браузерного режима на конкретной частной шине.
pub struct PlayerControl {
    bus_address: String,
}

impl PlayerControl {
    /// Пульт для шины с данным адресом (`unix:path=…`).
    pub fn on_bus(bus_address: &str) -> Self {
        Self {
            bus_address: bus_address.to_owned(),
        }
    }

    /// Ждёт, пока окно опубликует управляемый вид (`CanPause = true`; кнопки появляются
    /// вместе со страницей).
    pub fn wait_until_controllable(&self, timeout: Duration) -> Result<(), PlayerControlError> {
        let deadline = Instant::now() + timeout;
        loop {
            let ready = self
                .property(PLAYER_INTERFACE, "CanPause")
                .is_ok_and(|text| text.trim() == "b true");
            if ready {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(PlayerControlError::NotControllable(timeout));
            }
            std::thread::sleep(POLL_STEP);
        }
    }

    /// Ждёт, пока окно уйдёт с шины (процесс плеера вышел): имя MPRIS освобождается вместе с
    /// окном, и чтение свойства начинает падать. Звать только после того, как окно было
    /// управляемым, иначе «ещё не появилось» неотличимо от «уже ушло».
    pub fn wait_until_gone(&self, timeout: Duration) -> Result<(), PlayerControlError> {
        wait_until_absent(timeout, || {
            self.property(PLAYER_INTERFACE, "CanPause").is_ok()
        })
    }

    /// `Pause`.
    pub fn pause(&self) -> Result<(), PlayerControlError> {
        self.call(PLAYER_INTERFACE, "Pause", &[]).map(|_| ())
    }

    /// `Play`.
    pub fn play(&self) -> Result<(), PlayerControlError> {
        self.call(PLAYER_INTERFACE, "Play", &[]).map(|_| ())
    }

    /// `Seek` на `offset_micros` от текущей позиции (отрицательное — назад).
    pub fn seek_by(&self, offset_micros: i64) -> Result<(), PlayerControlError> {
        self.call(PLAYER_INTERFACE, "Seek", &["x", &offset_micros.to_string()])
            .map(|_| ())
    }

    /// Текущая позиция воспроизведения, микросекунды (`Position`).
    pub fn position_micros(&self) -> Result<i64, PlayerControlError> {
        let output = self.property(PLAYER_INTERFACE, "Position")?;
        parse_int64_property(&output).ok_or(PlayerControlError::UnexpectedOutput {
            property: "Position",
            output,
        })
    }

    /// `Quit` корневого интерфейса: закрыть окно, как закрыл бы человек.
    pub fn quit_window(&self) -> Result<(), PlayerControlError> {
        self.call(ROOT_INTERFACE, "Quit", &[]).map(|_| ())
    }

    fn property(&self, interface: &str, name: &'static str) -> Result<String, PlayerControlError> {
        self.busctl(&[
            "get-property",
            BROWSER_BUS_NAME,
            OBJECT_PATH,
            interface,
            name,
        ])
    }

    fn call(
        &self,
        interface: &str,
        method: &str,
        signature_and_arguments: &[&str],
    ) -> Result<String, PlayerControlError> {
        self.busctl(&call_arguments(interface, method, signature_and_arguments))
    }

    fn busctl(&self, arguments: &[&str]) -> Result<String, PlayerControlError> {
        let output = Command::new("busctl")
            .arg(format!("--address={}", self.bus_address))
            .arg("--no-pager")
            .args(arguments)
            .output()
            .map_err(|error| PlayerControlError::Spawn(error.to_string()))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(PlayerControlError::CommandFailed {
                command: arguments.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }
}

/// Опрашивает `is_present`, пока она не вернёт `false` (успех) или не выйдет срок.
fn wait_until_absent(
    timeout: Duration,
    mut is_present: impl FnMut() -> bool,
) -> Result<(), PlayerControlError> {
    let deadline = Instant::now() + timeout;
    loop {
        if !is_present() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(PlayerControlError::StillPresent(timeout));
        }
        std::thread::sleep(POLL_STEP);
    }
}

/// Аргументы `busctl call`. Перед сигнатурой и значениями стоит `--`: иначе `busctl` примет
/// отрицательное число (перемотка назад) за свою опцию («unrecognized option '-8'»).
fn call_arguments<'a>(
    interface: &'a str,
    method: &'a str,
    signature_and_arguments: &[&'a str],
) -> Vec<&'a str> {
    let mut arguments = vec!["call", BROWSER_BUS_NAME, OBJECT_PATH, interface, method];
    if !signature_and_arguments.is_empty() {
        arguments.push("--");
        arguments.extend_from_slice(signature_and_arguments);
    }
    arguments
}

/// Разбирает вывод `busctl get-property` для 64-битного целого: `x 12345`.
fn parse_int64_property(output: &str) -> Option<i64> {
    let (signature, value) = output.trim().split_once(' ')?;
    if signature != "x" {
        return None;
    }
    value.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_output_is_parsed_exactly() {
        assert_eq!(parse_int64_property("x 4200000\n"), Some(4_200_000));
        assert_eq!(parse_int64_property("x -5"), Some(-5));
    }

    #[test]
    fn a_negative_seek_is_protected_from_busctl_option_parsing() {
        let arguments = call_arguments(PLAYER_INTERFACE, "Seek", &["x", "-8000000"]);
        let separator = arguments
            .iter()
            .position(|word| *word == "--")
            .expect("a separator");
        assert_eq!(&arguments[separator + 1..], ["x", "-8000000"]);
        assert_eq!(arguments[0], "call");
        // Метод без аргументов обходится без разделителя.
        assert!(!call_arguments(PLAYER_INTERFACE, "Pause", &[]).contains(&"--"));
    }

    #[test]
    fn a_bus_without_the_window_counts_as_a_window_that_is_gone() {
        let control = PlayerControl::on_bus("unix:path=/nonexistent/fastiplayer-bench-bus");
        assert!(control.wait_until_gone(Duration::from_millis(300)).is_ok());
    }

    #[test]
    fn waiting_for_absence_succeeds_once_the_window_leaves_and_fails_if_it_stays() {
        let mut polls = 0;
        let left = wait_until_absent(Duration::from_secs(5), || {
            polls += 1;
            polls < 3
        });
        assert!(left.is_ok());
        assert_eq!(polls, 3);
        let stays = wait_until_absent(Duration::from_millis(300), || true);
        assert!(matches!(stays, Err(PlayerControlError::StillPresent(_))));
    }

    #[test]
    fn other_types_and_garbage_are_not_positions() {
        assert_eq!(parse_int64_property("b true"), None);
        assert_eq!(parse_int64_property("x notanumber"), None);
        assert_eq!(parse_int64_property(""), None);
    }

    #[test]
    fn a_bus_without_the_window_is_a_command_failure_not_a_panic() {
        // Адрес заведомо мёртвый: управление обязано вернуть ошибку, а не зависнуть и не паниковать.
        let control = PlayerControl::on_bus("unix:path=/nonexistent/fastiplayer-bench-bus");
        assert!(matches!(
            control.pause(),
            Err(PlayerControlError::CommandFailed { .. })
        ));
        assert!(matches!(
            control.wait_until_controllable(Duration::from_millis(300)),
            Err(PlayerControlError::NotControllable(_))
        ));
    }
}
