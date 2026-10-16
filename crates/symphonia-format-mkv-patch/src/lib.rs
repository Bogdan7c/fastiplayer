// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]
// The following lints are allowed in all Symphonia crates. Please see clippy.toml for their
// justification.
#![allow(clippy::comparison_chain)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_range_contains)]

mod attachment_limits;
mod codecs;
mod demuxer;
mod ebml;
mod lacing;
mod schema;
mod segment;
mod tags;
mod track_names;

#[cfg(test)]
mod reader_tests;

pub use crate::attachment_limits::{
    AttachmentReadLimits, SkippedAttachment, SkippedAttachmentReason, with_attachment_read_limits,
};
pub use crate::demuxer::MkvReader;

/// Ключ per-track тега с названием дорожки (`TrackEntry/Name`); `track_id` тега = номер дорожки.
pub const FASTIPLAYER_TRACK_NAME_TAG: &str = "fastiplayer.track.name";

pub mod sub_fields {
    //! Key name constants for sub-fields of MKV tags and chapters.
    //!
    //! For the exact meaning of these fields, and the format of their values, please consult the
    //! official Matroska specification.

    pub const TAG_LANGUAGE: &str = "LANGUAGE";
    pub const TAG_LANGUAGE_BCP47: &str = "LANGUAGE_BCP47";

    pub const CHAPTER_TITLE_COUNTRY: &str = "CHAPTER_TITLE_COUNTRY";
    pub const CHAPTER_TITLE_LANGUAGE: &str = "CHAPTER_TITLE_LANGUAGE";
    pub const CHAPTER_TITLE_LANGUAGE_BCP47: &str = "CHAPTER_TITLE_LANGUAGE_BCP47";

    pub const EDITION_TITLE_LANGUAGE_BCP47: &str = "EDITION_TITLE_LANGUAGE_BCP47";
}
