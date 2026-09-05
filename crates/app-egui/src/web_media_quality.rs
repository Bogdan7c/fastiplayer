//! Composition mapping между persisted config и neutral web-media policy.

use fastiplayer_config::{
    MAX_PREFERRED_VIDEO_HEIGHT as CONFIG_MAX_VIDEO_HEIGHT,
    PreferredVideoHeight as ConfigPreferredVideoHeight,
};
use web_media_core::{
    MAX_VIDEO_HEIGHT, PreferredHeightPolicy, PreferredVideoHeight as WebPreferredVideoHeight,
};

/// Compile-time guard не позволяет config и neutral contract незаметно разойтись по bounds.
const _: () = assert!(CONFIG_MAX_VIDEO_HEIGHT == MAX_VIDEO_HEIGHT);

/// Стартовая ступень автоматического качества.
///
/// 720p совпадает с исходным viewport приложения и не заставляет первый playback ждать тяжёлую
/// 1080p/4K rendition до того, как runtime успел получить evidence устойчивости сети.
pub(crate) const AUTOMATIC_STARTUP_VIDEO_HEIGHT_PIXELS: u32 = 720;

/// Преобразует persisted global preference в neutral selection policy.
///
/// `None` означает автоматический fast-start. Явная высота остаётся строгим user preference.
pub(crate) fn preferred_height_policy(
    preferred_height: Option<ConfigPreferredVideoHeight>,
) -> PreferredHeightPolicy {
    match preferred_height {
        None => {
            let startup_height =
                WebPreferredVideoHeight::new(AUTOMATIC_STARTUP_VIDEO_HEIGHT_PIXELS)
                    .expect("automatic startup height входит в neutral bounds");
            PreferredHeightPolicy::Prefer(startup_height)
        }
        Some(preferred_height) => {
            let neutral_height = WebPreferredVideoHeight::new(preferred_height.pixels())
                .expect("compile-time synchronized bounds гарантируют infallible mapping");
            PreferredHeightPolicy::Prefer(neutral_height)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_uses_latency_first_automatic_startup_height() {
        let PreferredHeightPolicy::Prefer(startup_height) = preferred_height_policy(None) else {
            panic!("automatic mode должен иметь bounded startup ступень");
        };
        assert_eq!(
            startup_height.height().pixels(),
            AUTOMATIC_STARTUP_VIDEO_HEIGHT_PIXELS
        );
    }

    #[test]
    fn validated_config_height_maps_to_same_neutral_pixels() {
        let config_height = ConfigPreferredVideoHeight::new(2160).expect("2160 валидно");
        let PreferredHeightPolicy::Prefer(neutral_height) =
            preferred_height_policy(Some(config_height))
        else {
            panic!("configured height должна включить preferred-height policy");
        };
        assert_eq!(neutral_height.height().pixels(), 2160);
    }
}
