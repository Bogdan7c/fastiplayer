//! Содержимое `.service`-файла D-Bus activation для браузерного режима.
//!
//! Файл кладётся в `~/.local/share/dbus-1/services/<имя службы>.service`; по нему
//! шина запускает `fastiplayer --browser-session`, когда посредник звонит на имя
//! службы, а процесса ещё нет.
//!
//! # Как разбирается `Exec=` (почему кавычки именно такие)
//!
//! - Референсный `dbus-daemon` режет значение `Exec` оболочечным разбором
//!   (`_dbus_shell_parse_argv`, происходит от `g_shell_parse_argv`): пробел
//!   разделяет аргументы, одинарные кавычки сохраняют всё буквально.
//! - `dbus-broker` (запуск активации у systemd-сеансов) с версии 17 разбирает `Exec`
//!   библиотекой `c-shquote`, «POSIX-совместимым разбором кавычек оболочки»
//!   (NEWS dbus-broker 17), а ключи читает простым парсером `c-ini`.
//!
//! - Проверено на настоящем `dbus-daemon` (опыт при написании теста активации): файл
//!   с апострофом, записанным как `'\''`, шина молча не активирует (ServiceUnknown),
//!   вероятно потому, что её парсер ключей сам трактует `\` как escape, а
//!   `dbus-broker` (простой `c-ini`) этого бы не делал. Единой записи апострофа и
//!   обратной черты для обоих нет.
//!
//! Поэтому путь оборачивается в одинарные кавычки (пробелы внутри остаются частью
//! аргумента в обоих разборах), а пути с символами без общего толкования
//! отклоняются типизированной ошибкой: управляющие символы (перевод строки оборвал
//! бы строку ключа), обратная косая черта и апостроф. Пробел в пути разрешён и
//! закреплён тестом активации на настоящей частной шине.

use std::path::Path;

use thiserror::Error;

use super::{BROWSER_SESSION_ARGUMENT, BROWSER_SESSION_BUS_NAME};

/// Почему `.service`-файл для этого пути создать нельзя.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ActivationServiceFileError {
    /// Путь не абсолютный: шина запускает службу без привычного `PATH` и каталога.
    #[error("The path to the executable file must be absolute")]
    NotAbsolute,
    /// Путь не UTF-8: файл службы текстовый и такой путь записать не может.
    #[error("The path to the executable file is not valid UTF-8")]
    NotUtf8,
    /// В пути символ, который нельзя записать в `Exec=` однозначно.
    #[error("The path to the executable contains an invalid character: {character:?}")]
    UnsupportedCharacter {
        /// Первый недопустимый символ.
        character: char,
    },
}

/// Собирает содержимое `io.github.Bogdan7c.Fastiplayer.BrowserSession.service`.
pub fn browser_session_activation_service_file(
    executable: &Path,
) -> Result<String, ActivationServiceFileError> {
    if !executable.is_absolute() {
        return Err(ActivationServiceFileError::NotAbsolute);
    }
    let executable_text = executable
        .to_str()
        .ok_or(ActivationServiceFileError::NotUtf8)?;
    if let Some(character) = executable_text
        .chars()
        .find(|character| character.is_control() || matches!(character, '\\' | '\''))
    {
        return Err(ActivationServiceFileError::UnsupportedCharacter { character });
    }
    Ok(format!(
        "[D-BUS Service]\nName={BROWSER_SESSION_BUS_NAME}\nExec={} {BROWSER_SESSION_ARGUMENT}\n",
        single_quote_for_shell(executable_text)
    ))
}

/// Оборачивает текст в одинарные кавычки оболочки. Внутри `'` нет (см. проверку выше).
fn single_quote_for_shell(text: &str) -> String {
    format!("'{text}'")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{ActivationServiceFileError, browser_session_activation_service_file};

    #[test]
    fn plain_path_produces_quoted_exec_line_with_browser_session_argument() {
        let content =
            browser_session_activation_service_file(Path::new("/opt/fastiplayer/fastiplayer"))
                .expect("absolute path");
        assert_eq!(
            content,
            "[D-BUS Service]\n\
             Name=io.github.Bogdan7c.Fastiplayer.BrowserSession\n\
             Exec='/opt/fastiplayer/fastiplayer' --browser-session\n"
        );
    }

    #[test]
    fn spaces_stay_inside_one_quoted_argument() {
        let content =
            browser_session_activation_service_file(Path::new("/home/user/my player/bin"))
                .expect("absolute path");
        assert!(
            content.contains("Exec='/home/user/my player/bin' --browser-session"),
            "{content}"
        );
    }

    #[test]
    fn unusable_paths_are_rejected_with_typed_errors() {
        assert_eq!(
            browser_session_activation_service_file(Path::new("relative/fastiplayer")),
            Err(ActivationServiceFileError::NotAbsolute)
        );
        assert_eq!(
            browser_session_activation_service_file(Path::new("/opt/new\nline")),
            Err(ActivationServiceFileError::UnsupportedCharacter { character: '\n' })
        );
        assert_eq!(
            browser_session_activation_service_file(Path::new("/opt/o'neil")),
            Err(ActivationServiceFileError::UnsupportedCharacter { character: '\'' })
        );
        assert_eq!(
            browser_session_activation_service_file(Path::new(r"/opt/back\slash")),
            Err(ActivationServiceFileError::UnsupportedCharacter { character: '\\' })
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_path_is_rejected() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(OsStr::from_bytes(b"/opt/\xFF/fastiplayer"));
        assert_eq!(
            browser_session_activation_service_file(path),
            Err(ActivationServiceFileError::NotUtf8)
        );
    }
}
