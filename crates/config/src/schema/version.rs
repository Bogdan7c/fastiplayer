/// Текущая версия TOML-схемы.
pub const CURRENT_SCHEMA_VERSION: u32 = 23;

/// Старая схема до публичного выбора `auto`/`hardware`/`software`.
pub(crate) const LEGACY_SCHEMA_VERSION_2: u32 = 2;

/// Старая схема с уже удалённой галкой `video.hardware_decode_only`.
pub(crate) const LEGACY_SCHEMA_VERSION_3: u32 = 3;

/// Старая схема, где `[frame_server]` ещё содержал hover/predecode knobs.
pub(crate) const LEGACY_SCHEMA_VERSION_4: u32 = 4;

/// Последняя схема с секцией `[youtube]` и placeholder-полем account session.
pub(crate) const LEGACY_SCHEMA_VERSION_5: u32 = 5;

/// Старая схема до глобальной preferred video height.
pub(crate) const LEGACY_SCHEMA_VERSION_6: u32 = 6;

/// Старая схема до configurable VOD endpoint recovery policy.
pub(crate) const LEGACY_SCHEMA_VERSION_7: u32 = 7;

/// Старая схема до bounded next-item source/demux preload policy.
pub(crate) const LEGACY_SCHEMA_VERSION_8: u32 = 8;

/// Последняя схема, где web-media policy находилась внутри `[yt_dlp]`.
pub(crate) const LEGACY_SCHEMA_VERSION_9: u32 = 9;

/// Последняя схема, где `playlist.error_behavior` по умолчанию был `stop`.
///
/// Решение владельца (UX edge cases, сессия 07): при переходе на v11 сохранённый
/// `stop` один раз меняется на `skip`, потому что default-документ записывал значение
/// явно и отличить «выбрал сам» от «осталось по умолчанию» невозможно.
pub(crate) const LEGACY_SCHEMA_VERSION_10: u32 = 10;

/// Последняя схема до настроек drag & drop (`playlist.dropped_*`).
///
/// Решение владельца (UX edge cases, сессия 12): три новых поля добавляются со
/// значениями по умолчанию; перезаписывать пользовательские значения нечего, поэтому
/// миграция v11 -> v12 только поднимает версию (старый файл без полей грузится с defaults).
pub(crate) const LEGACY_SCHEMA_VERSION_11: u32 = 11;

/// Последняя схема до секции `[subtitles]`.
///
/// Решение владельца (субтитры, сессия 04): секция добавляется целиком со значениями по
/// умолчанию; переписывать пользовательские значения нечего, поэтому миграция v12 -> v13
/// только поднимает версию (старый файл без секции грузится с defaults).
pub(crate) const LEGACY_SCHEMA_VERSION_12: u32 = 12;

/// Последняя схема до позиции субтитров и флага `ass_force_user_style`.
///
/// Решение владельца (субтитры, сессия 05): три поля добавляются со значениями по
/// умолчанию; переписывать пользовательские значения нечего, поэтому миграция v13 -> v14
/// только поднимает версию (старый файл без полей грузится с defaults).
pub(crate) const LEGACY_SCHEMA_VERSION_13: u32 = 13;

/// Последняя схема до лимитов встроенных дорожек субтитров.
///
/// Решение владельца (субтитры, сессия 06): четыре лимита (вложенные шрифты, труба
/// пакетов, показ реплики без длительности) добавляются со значениями по умолчанию;
/// переписывать пользовательские значения нечего, поэтому миграция v14 -> v15 только
/// поднимает версию (старый файл без ключей грузится с defaults).
pub(crate) const LEGACY_SCHEMA_VERSION_14: u32 = 14;

/// Последняя схема до ключа `playlist.track_memory_max_entries`.
///
/// Решение владельца (субтитры, сессия 07): один лимит памяти дорожек добавляется со
/// значением по умолчанию; переписывать пользовательские значения нечего, поэтому
/// миграция v15 -> v16 только поднимает версию (старый файл без ключа грузится с defaults).
pub(crate) const LEGACY_SCHEMA_VERSION_15: u32 = 15;

/// Последняя схема до ключа `audio.startup_read_ahead_cap_ms`.
///
/// Решение владельца: потолок чтения аудио вперёд до первого видеокадра добавляется со
/// значением по умолчанию; переписывать пользовательские значения нечего, поэтому
/// миграция v16 -> v17 только поднимает версию (старый файл без ключа грузится с defaults).
pub(crate) const LEGACY_SCHEMA_VERSION_16: u32 = 16;

/// Последняя схема до ASS-настроек (`subtitles.ass_position_mode`, лимиты разбора ASS).
///
/// Решение владельца (субтитры, сессия 08): четыре ключа добавляются со значениями по
/// умолчанию; переписывать пользовательские значения нечего, поэтому миграция v17 -> v18
/// только поднимает версию (старый файл без ключей грузится с defaults).
pub(crate) const LEGACY_SCHEMA_VERSION_17: u32 = 17;

/// Последняя схема до настроек упреждающей отрисовки субтитров
/// (`subtitles.prerender_frames`, `subtitles.prepared_overlays`).
///
/// Новые ключи добавляются со значениями по умолчанию; переписывать пользовательские
/// значения нечего, поэтому миграция v18 -> v19 только поднимает версию.
pub(crate) const LEGACY_SCHEMA_VERSION_18: u32 = 18;

/// Последняя схема до настройки удерживаемой памяти размытия субтитров
/// (`subtitles.blur_scratch_retained_mb`).
///
/// Новый ключ добавляется со значением по умолчанию (равным прежней константе растеризатора);
/// переписывать пользовательские значения нечего, поэтому миграция v19 -> v20 только поднимает
/// версию.
pub(crate) const LEGACY_SCHEMA_VERSION_19: u32 = 19;

/// Последняя схема до настроек кэшей глифов растеризатора субтитров
/// (`subtitles.glyph_cache_mb`, `subtitles.glyph_outline_cache_items`).
///
/// Новые ключи добавляются со значениями по умолчанию; переписывать пользовательские значения
/// нечего, поэтому миграция v20 -> v21 только поднимает версию.
pub(crate) const LEGACY_SCHEMA_VERSION_20: u32 = 20;

/// Последняя схема до настроек памяти встроенных дорожек PGS
/// (`subtitles.pgs_displayed_track_cache_mb`, `subtitles.pgs_background_track_window_s`).
///
/// Новые ключи добавляются со значениями по умолчанию; переписывать пользовательские значения
/// нечего, поэтому миграция v21 -> v22 только поднимает версию.
pub(crate) const LEGACY_SCHEMA_VERSION_21: u32 = 21;

/// Последняя схема до лимитов субтитров потоковых источников (`[subtitles.stream]`).
///
/// Новая вложенная секция читается со значениями по умолчанию; переписывать пользовательские
/// значения нечего, поэтому миграция v22 -> v23 только поднимает версию.
pub(crate) const LEGACY_SCHEMA_VERSION_22: u32 = 22;

#[cfg(test)]
mod tests {
    use super::*;

    /// Текущая схема v23 и полная цепочка поддерживаемых legacy-версий v2..v22.
    #[test]
    fn schema_v23_and_supported_legacy_versions_are_stable() {
        assert_eq!(CURRENT_SCHEMA_VERSION, 23);
        assert_eq!(
            [
                LEGACY_SCHEMA_VERSION_2,
                LEGACY_SCHEMA_VERSION_3,
                LEGACY_SCHEMA_VERSION_4,
                LEGACY_SCHEMA_VERSION_5,
                LEGACY_SCHEMA_VERSION_6,
                LEGACY_SCHEMA_VERSION_7,
                LEGACY_SCHEMA_VERSION_8,
                LEGACY_SCHEMA_VERSION_9,
                LEGACY_SCHEMA_VERSION_10,
                LEGACY_SCHEMA_VERSION_11,
                LEGACY_SCHEMA_VERSION_12,
                LEGACY_SCHEMA_VERSION_13,
                LEGACY_SCHEMA_VERSION_14,
                LEGACY_SCHEMA_VERSION_15,
                LEGACY_SCHEMA_VERSION_16,
                LEGACY_SCHEMA_VERSION_17,
                LEGACY_SCHEMA_VERSION_18,
                LEGACY_SCHEMA_VERSION_19,
                LEGACY_SCHEMA_VERSION_20,
                LEGACY_SCHEMA_VERSION_21,
                LEGACY_SCHEMA_VERSION_22,
            ],
            [
                2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22
            ]
        );
    }
}
