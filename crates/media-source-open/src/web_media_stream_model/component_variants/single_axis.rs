//! Пулы одноосевого выбора (`VideoOnly` / `AudioOnly`) в Topology-каталоге.
//!
//! В Topology-каталоге строка video (audio) может существовать только как часть пары
//! `VideoAndAudio`. Как самостоятельный выбор допустимы лишь строки из списков
//! `video_only` / `audio_only` (так решает `web-media-core`:
//! `is_video_only_selectable` / `is_audio_only_selectable`, замена строки проверяется тем же
//! правилом). Поэтому sidebar при одноосевом выборе обязан показывать именно этот отфильтрованный
//! пул, а индекс варианта в действиях и в `replace` считается внутри него, а не во всём списке.
//!
//! Для плоских каталогов `VideoOnly` / `AudioOnly` каждая строка выбираема, и фильтр
//! возвращает весь список без изменений.

use web_media_core::{
    AudioComponentVariant, ComponentVariantCatalog, ComponentVariantError, VideoComponentVariant,
};

/// Возвращает video-строки, которые каталог разрешает выбрать как `VideoOnly`, в порядке каталога.
pub(super) fn video_only_pool(
    catalog: &ComponentVariantCatalog,
) -> Result<Vec<&VideoComponentVariant>, ComponentVariantError> {
    Ok(catalog
        .required_video_variants()?
        .iter()
        .filter(|variant| catalog.is_video_only_selectable(variant.exact_identity()))
        .collect())
}

/// Возвращает audio-строки, которые каталог разрешает выбрать как `AudioOnly`, в порядке каталога.
pub(super) fn audio_only_pool(
    catalog: &ComponentVariantCatalog,
) -> Result<Vec<&AudioComponentVariant>, ComponentVariantError> {
    Ok(catalog
        .required_audio_variants()?
        .iter()
        .filter(|variant| catalog.is_audio_only_selectable(variant.exact_identity()))
        .collect())
}
