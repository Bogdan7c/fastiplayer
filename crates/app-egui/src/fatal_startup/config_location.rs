//! Как показать путь к папке настроек в тексте ошибки и в команде для терминала.
//!
//! Решение владельца (UX06): путь к папке настроек показывать можно — это не путь
//! к медиафайлу пользователя. Домашняя папка сокращается до `~`, чтобы текст был
//! короче и не раскрывал имя учётной записи на скриншоте.

use std::path::{Path, PathBuf};

/// Путь к папке настроек в двух видах: для чтения и для вставки в терминал.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigDirectoryLocation {
    /// Например `~/.config/fastiplayer`.
    readable_text: String,
    /// То же, но безопасно для shell: пробелы и спецсимволы взяты в кавычки.
    shell_argument: String,
}

impl ConfigDirectoryLocation {
    /// Путь для текущего пользователя: домашняя папка берётся из `HOME`.
    pub(crate) fn for_current_user(config_dir: &Path) -> Self {
        let home_dir = std::env::var_os("HOME").map(PathBuf::from);
        Self::relative_to_home(config_dir, home_dir.as_deref())
    }

    /// Строит оба представления; `home_dir` передаётся явно ради тестов.
    pub(crate) fn relative_to_home(config_dir: &Path, home_dir: Option<&Path>) -> Self {
        match path_inside_home(config_dir, home_dir) {
            Some(relative_path) => {
                let relative_text = relative_path.to_string_lossy();
                Self {
                    readable_text: format!("~/{relative_text}"),
                    // `~` должен остаться вне кавычек, иначе shell его не раскроет.
                    shell_argument: format!("~/{}", shell_quoted(&relative_text)),
                }
            }
            None => {
                let full_text = config_dir.to_string_lossy();
                Self {
                    readable_text: full_text.to_string(),
                    shell_argument: shell_quoted(&full_text),
                }
            }
        }
    }

    /// Путь для текста сообщения.
    pub(crate) fn readable_text(&self) -> &str {
        &self.readable_text
    }

    /// Путь для подстановки в команду терминала.
    pub(crate) fn shell_argument(&self) -> &str {
        &self.shell_argument
    }
}

/// Часть пути внутри домашней папки, если она там есть.
///
/// Пустой `HOME` и `HOME=/` не считаются домашней папкой: иначе любой путь
/// превратился бы в `~/…` и ввёл бы пользователя в заблуждение.
fn path_inside_home<'path>(
    config_dir: &'path Path,
    home_dir: Option<&Path>,
) -> Option<&'path Path> {
    let home_dir = home_dir.filter(|home| home.parent().is_some())?;
    config_dir
        .strip_prefix(home_dir)
        .ok()
        .filter(|relative_path| !relative_path.as_os_str().is_empty())
}

/// Возвращает слово как есть, если в нём только безопасные для shell символы,
/// иначе берёт его в одинарные кавычки (сама кавычка внутри — `'\''`).
fn shell_quoted(word: &str) -> String {
    let is_shell_safe = !word.is_empty()
        && word
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "/._-+,:@%".contains(character));
    if is_shell_safe {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::ConfigDirectoryLocation;

    #[test]
    fn directory_inside_home_is_shortened_to_tilde() {
        let location = ConfigDirectoryLocation::relative_to_home(
            Path::new("/home/user/.config/fastiplayer"),
            Some(Path::new("/home/user")),
        );

        assert_eq!(location.readable_text(), "~/.config/fastiplayer");
        assert_eq!(location.shell_argument(), "~/.config/fastiplayer");
    }

    #[test]
    fn directory_outside_home_keeps_full_path() {
        let location = ConfigDirectoryLocation::relative_to_home(
            Path::new("/srv/config/fastiplayer"),
            Some(Path::new("/home/user")),
        );

        assert_eq!(location.readable_text(), "/srv/config/fastiplayer");
        assert_eq!(location.shell_argument(), "/srv/config/fastiplayer");
    }

    #[test]
    fn root_or_missing_home_is_not_treated_as_home() {
        for home_dir in [Some(Path::new("/")), Some(Path::new("")), None] {
            let location =
                ConfigDirectoryLocation::relative_to_home(Path::new("/etc/fastiplayer"), home_dir);

            assert_eq!(location.readable_text(), "/etc/fastiplayer", "{home_dir:?}");
        }
    }

    #[test]
    fn similar_prefix_is_not_mistaken_for_home() {
        let location = ConfigDirectoryLocation::relative_to_home(
            Path::new("/home/user2/.config/fastiplayer"),
            Some(Path::new("/home/user")),
        );

        assert_eq!(location.readable_text(), "/home/user2/.config/fastiplayer");
    }

    #[test]
    fn spaces_and_quotes_are_quoted_for_shell_but_not_for_reading() {
        let location = ConfigDirectoryLocation::relative_to_home(
            Path::new("/home/user/my config/it's fastiplayer"),
            Some(Path::new("/home/user")),
        );

        assert_eq!(location.readable_text(), "~/my config/it's fastiplayer");
        assert_eq!(
            location.shell_argument(),
            r"~/'my config/it'\''s fastiplayer'"
        );
    }
}
