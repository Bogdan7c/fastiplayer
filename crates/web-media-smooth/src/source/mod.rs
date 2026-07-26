//! Lazy finite fragment sources поверх приватного provider runtime seed.

mod audio;
mod cursor;
mod error;
mod policy;
mod selection;
#[cfg(test)]
pub(crate) mod tests;
mod video;

pub use audio::SmoothAudioFragmentSource;
pub use error::SmoothFragmentSourceBuildError;
pub use policy::SmoothFragmentSourcePolicy;
pub(crate) use selection::{
    SmoothBuiltSourcePair, SmoothSelectedSourceFactory, build_audio_probe_source,
    build_video_probe_source,
};
pub use selection::{SmoothFragmentSourceParts, SmoothSelectedFragmentSources};
pub use video::SmoothVideoFragmentSource;
