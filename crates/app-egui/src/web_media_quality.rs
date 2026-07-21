//! Composition mapping между persisted config и neutral web-media policy.

use rustiplayer_config::{
    MAX_PREFERRED_VIDEO_HEIGHT as CONFIG_MAX_VIDEO_HEIGHT,
    PreferredVideoHeight as ConfigPreferredVideoHeight,
};
use web_media_core::{
    MAX_VIDEO_HEIGHT, PreferredHeightPolicy, PreferredVideoHeight as WebPreferredVideoHeight,
};

/// Compile-time guard не позволяет config и neutral contract незаметно разойтись по bounds.
const _: () = assert!(CONFIG_MAX_VIDEO_HEIGHT == MAX_VIDEO_HEIGHT);

/// Преобразует persisted global preference в neutral selection policy.
///
/// `None` означает обычный `BestPlayable`: height не участвует в ordering.
pub(crate) fn preferred_height_policy(
    preferred_height: Option<ConfigPreferredVideoHeight>,
) -> PreferredHeightPolicy {
    match preferred_height {
        None => PreferredHeightPolicy::NoPreference,
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
    fn none_keeps_best_playable_without_height_ranking() {
        assert_eq!(
            preferred_height_policy(None),
            PreferredHeightPolicy::NoPreference
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
