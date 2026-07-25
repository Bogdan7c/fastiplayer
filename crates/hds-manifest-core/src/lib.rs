//! Bounded, I/O-free HDS F4M and bootstrap model.
//!
//! Этот crate знает только форматные инварианты. Он не открывает URL, не
//! выбирает rendition и не владеет HTTP lifecycle: это остаётся у S38 runtime.

#![forbid(unsafe_code)]

mod bootstrap;
mod model;
mod parser;

pub use bootstrap::{
    HdsBootstrapError, HdsBootstrapLimits, HdsBootstrapTimeline, HdsFragment, HdsFragmentRun,
    HdsSegmentRun, parse_bootstrap,
};
pub use model::{
    F4mBootstrapInfo, F4mBootstrapSource, F4mManifest, F4mManifestLimits, F4mMediaEntry,
    F4mStreamType,
};
pub use parser::{F4mManifestError, parse_f4m_manifest};
