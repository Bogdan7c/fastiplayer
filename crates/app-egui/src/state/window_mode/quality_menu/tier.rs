//! Подпись качества по размеру картинки: «1080p», «720p», …
//!
//! Подпись — не высота в пикселях, а **ступень**: самая маленькая стандартная ступень `T`,
//! в чей кадр 16:9 (`T·16/9 × T`, в любой ориентации) помещается картинка. Так широкий
//! фильм 1920×800 — «1080p», а вертикальное 1080×1920 — тоже «1080p», как их называют сайты.
//! Совсем маленькие (короче 144 по короткой стороне) и огромные (больше кадра 4320p)
//! картинки не подгоняются под ступень и подписываются короткой стороной.

use super::ShownPicture;

/// Стандартные ступени качества по возрастанию (высота кадра 16:9).
const STANDARD_TIERS: [u32; 9] = [144, 240, 360, 480, 720, 1080, 1440, 2160, 4320];

/// Подпись ступени для картинки.
pub(super) fn tier_label(picture: ShownPicture) -> String {
    let long_side = u64::from(picture.width.max(picture.height));
    let short_side = u64::from(picture.width.min(picture.height));
    let smallest_tier = u64::from(STANDARD_TIERS[0]);
    if short_side < smallest_tier {
        return format!("{short_side}p");
    }
    // Кадр ступени `T`: короткая сторона T, длинная T·16/9, округлённая ВВЕРХ: настоящий
    // 480p-ролик — 854×480 (а не 853,33×480).
    let fits_tier = |tier: u32| {
        let tier = u64::from(tier);
        short_side <= tier && long_side <= (tier * 16).div_ceil(9)
    };
    match STANDARD_TIERS.into_iter().find(|tier| fits_tier(*tier)) {
        Some(tier) => format!("{tier}p"),
        None => format!("{short_side}p"),
    }
}
