//! Выбор файлов, которые нужны динамическому загрузчику.

/// Проверяет форму `lib<имя>.so.<целое число>` — SONAME-имя библиотеки.
///
/// Загрузчик ищет именно такие имена (их записывает линковщик в `NEEDED`).
/// Полное имя версии `libavcodec.so.62.28.103` и dev-ссылка `libavcodec.so`
/// рядом с плеером не нужны.
#[must_use]
pub fn is_soname_file_name(file_name: &str) -> bool {
    let Some((stem, major_version)) = file_name.rsplit_once(".so.") else {
        return false;
    };
    stem.starts_with("lib")
        && !major_version.is_empty()
        && major_version.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::is_soname_file_name;

    #[test]
    fn soname_files_are_selected() {
        assert!(is_soname_file_name("libavcodec.so.62"));
        assert!(is_soname_file_name("libavutil.so.60"));
    }

    #[test]
    fn full_version_dev_link_and_foreign_files_are_skipped() {
        // Полное имя версии и dev-ссылка загрузчику не нужны.
        assert!(!is_soname_file_name("libavcodec.so.62.28.103"));
        assert!(!is_soname_file_name("libavcodec.so"));
        // Посторонние файлы в lib/ не копируются рядом с плеером.
        assert!(!is_soname_file_name("pkgconfig"));
        assert!(!is_soname_file_name("avcodec.so.62"));
        assert!(!is_soname_file_name("libavcodec.so."));
    }
}
