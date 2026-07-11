/// Текущая версия TOML-схемы.
pub const CURRENT_SCHEMA_VERSION: u32 = 5;

/// Старая схема до публичного выбора `auto`/`hardware`/`software`.
pub(crate) const LEGACY_SCHEMA_VERSION_2: u32 = 2;

/// Старая схема с уже удалённой галкой `video.hardware_decode_only`.
pub(crate) const LEGACY_SCHEMA_VERSION_3: u32 = 3;

/// Старая схема, где `[frame_server]` ещё содержал hover/predecode knobs.
pub(crate) const LEGACY_SCHEMA_VERSION_4: u32 = 4;

#[cfg(test)]
mod tests {
    use super::*;

    /// Session 23 не имеет права повышать current schema или менять legacy chain.
    #[test]
    fn schema_v5_and_supported_legacy_versions_are_stable() {
        assert_eq!(CURRENT_SCHEMA_VERSION, 5);
        assert_eq!(
            [
                LEGACY_SCHEMA_VERSION_2,
                LEGACY_SCHEMA_VERSION_3,
                LEGACY_SCHEMA_VERSION_4
            ],
            [2, 3, 4]
        );
    }
}
