/// Текущая версия TOML-схемы.
pub const CURRENT_SCHEMA_VERSION: u32 = 11;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Текущая схема v11 и полная цепочка поддерживаемых legacy-версий v2..v10.
    #[test]
    fn schema_v11_and_supported_legacy_versions_are_stable() {
        assert_eq!(CURRENT_SCHEMA_VERSION, 11);
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
            ],
            [2, 3, 4, 5, 6, 7, 8, 9, 10]
        );
    }
}
