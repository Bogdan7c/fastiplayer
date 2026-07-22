use std::fmt;

use serde::{Deserialize, Serialize};

use crate::VideoCodec;

/// Профиль VP9 из bitstream header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp9Profile {
    /// VP9 Profile 0: 8-bit 4:2:0.
    Profile0,

    /// VP9 Profile 1: 8-bit 4:2:2 или 4:4:4.
    Profile1,

    /// VP9 Profile 2: 10/12-bit 4:2:0.
    Profile2,

    /// VP9 Profile 3: 10/12-bit 4:2:2 или 4:4:4.
    Profile3,
}

impl Vp9Profile {
    /// Переводит числовое значение из VP9 uncompressed header в typed profile.
    #[must_use]
    pub const fn from_bitstream_value(profile: u8) -> Option<Self> {
        match profile {
            0 => Some(Self::Profile0),
            1 => Some(Self::Profile1),
            2 => Some(Self::Profile2),
            3 => Some(Self::Profile3),
            _ => None,
        }
    }
}

impl fmt::Display for Vp9Profile {
    /// Возвращает короткое имя profile для report/UI.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Profile0 => "Profile 0",
            Self::Profile1 => "Profile 1",
            Self::Profile2 => "Profile 2",
            Self::Profile3 => "Profile 3",
        };
        formatter.write_str(label)
    }
}

/// Профиль AV1, который VA-API может объявить для decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Av1Profile {
    /// AV1 Main profile.
    Main,

    /// AV1 High profile.
    High,

    /// AV1 Professional profile.
    Professional,
}

impl fmt::Display for Av1Profile {
    /// Возвращает короткое имя profile для report/UI.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Main => "Main",
            Self::High => "High",
            Self::Professional => "Professional",
        };
        formatter.write_str(label)
    }
}

/// Профиль H.264/AVC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H264Profile {
    /// Baseline profile без constrained-baseline ограничения.
    Baseline,

    /// Constrained Baseline profile.
    ConstrainedBaseline,

    /// Main profile.
    Main,

    /// High profile.
    High,
}

impl fmt::Display for H264Profile {
    /// Возвращает короткое имя profile для report/UI.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Baseline => "Baseline",
            Self::ConstrainedBaseline => "Constrained Baseline",
            Self::Main => "Main",
            Self::High => "High",
        };
        formatter.write_str(label)
    }
}

/// Профиль H.265/HEVC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H265Profile {
    /// Main 8-bit 4:2:0.
    Main,

    /// Main 10-bit 4:2:0.
    Main10,

    /// Main 12-bit 4:2:0.
    Main12,

    /// Main 10-bit 4:2:2.
    Main422_10,

    /// Main 12-bit 4:2:2.
    Main422_12,

    /// Main 8-bit 4:4:4.
    Main444,

    /// Main 10-bit 4:4:4.
    Main444_10,

    /// Main 12-bit 4:4:4.
    Main444_12,

    /// Screen Content Coding Main.
    SccMain,

    /// Screen Content Coding Main 10.
    SccMain10,

    /// Screen Content Coding Main 4:4:4.
    SccMain444,

    /// Screen Content Coding Main 4:4:4 10-bit.
    SccMain444_10,
}

impl fmt::Display for H265Profile {
    /// Возвращает короткое имя profile для report/UI.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Main => "Main",
            Self::Main10 => "Main 10",
            Self::Main12 => "Main 12",
            Self::Main422_10 => "Main 4:2:2 10",
            Self::Main422_12 => "Main 4:2:2 12",
            Self::Main444 => "Main 4:4:4",
            Self::Main444_10 => "Main 4:4:4 10",
            Self::Main444_12 => "Main 4:4:4 12",
            Self::SccMain => "SCC Main",
            Self::SccMain10 => "SCC Main 10",
            Self::SccMain444 => "SCC Main 4:4:4",
            Self::SccMain444_10 => "SCC Main 4:4:4 10",
        };
        formatter.write_str(label)
    }
}

/// Профиль VP8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp8Profile {
    /// VA-API объединяет VP8 version 0..3 в один decode profile.
    Version0To3,
}

impl fmt::Display for Vp8Profile {
    /// Возвращает короткое имя profile для report/UI.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Version0To3 => formatter.write_str("Version 0..3"),
        }
    }
}

/// Codec-specific video profile без строковой магии.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoProfile {
    /// VP9 profile.
    Vp9(Vp9Profile),

    /// AV1 profile.
    Av1(Av1Profile),

    /// H.264 profile.
    H264(H264Profile),

    /// H.265 profile.
    H265(H265Profile),

    /// VP8 profile.
    Vp8(Vp8Profile),
}

impl VideoProfile {
    /// Возвращает codec, которому принадлежит profile.
    #[must_use]
    pub const fn codec(self) -> VideoCodec {
        match self {
            Self::Vp9(_) => VideoCodec::Vp9,
            Self::Av1(_) => VideoCodec::Av1,
            Self::H264(_) => VideoCodec::H264,
            Self::H265(_) => VideoCodec::H265,
            Self::Vp8(_) => VideoCodec::Vp8,
        }
    }
}

impl fmt::Display for VideoProfile {
    /// Форматирует profile вместе с codec-контекстом.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vp9(profile) => write!(formatter, "VP9 {profile}"),
            Self::Av1(profile) => write!(formatter, "AV1 {profile}"),
            Self::H264(profile) => write!(formatter, "H.264 {profile}"),
            Self::H265(profile) => write!(formatter, "H.265 {profile}"),
            Self::Vp8(profile) => write!(formatter, "VP8 {profile}"),
        }
    }
}
