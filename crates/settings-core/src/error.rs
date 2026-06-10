use std::error::Error;
use std::fmt;

use crate::metadata::{SettingId, SettingValueKind, SettingValueType};
use crate::options::SettingOptionId;

/// Result type used by neutral settings-core APIs.
pub type SettingsResult<T> = Result<T, SettingsError>;

/// Registry/accessor level error.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsError {
    /// A registry attempted to register the same id twice.
    DuplicateSettingId { id: SettingId },

    /// Caller requested a setting id not present in the registry.
    UnknownSetting { id: SettingId },

    /// Caller attempted to mutate a read-only setting.
    ReadOnlySetting { id: SettingId },

    /// Caller attempted to reset a setting that has no reset behavior.
    ResetUnavailable { id: SettingId },

    /// Value failed descriptor-level validation.
    InvalidValue {
        /// Setting whose value failed.
        id: SettingId,

        /// Precise validation reason.
        reason: SettingValueError,
    },

    /// Accessor failed while reading, writing or resetting the document.
    AccessFailed {
        /// Setting id if the registry could attach one.
        id: Option<SettingId>,

        /// Human-readable diagnostic context.
        message: String,
    },
}

impl SettingsError {
    /// Creates an accessor failure without revealing storage internals.
    #[must_use]
    pub fn access_failed(message: impl Into<String>) -> Self {
        Self::AccessFailed {
            id: None,
            message: message.into(),
        }
    }

    pub(crate) fn with_setting_id(self, setting_id: &SettingId) -> Self {
        match self {
            Self::AccessFailed { id: None, message } => Self::AccessFailed {
                id: Some(setting_id.clone()),
                message,
            },
            other => other,
        }
    }
}

impl fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateSettingId { id } => write!(formatter, "duplicate setting id `{id}`"),
            Self::UnknownSetting { id } => write!(formatter, "unknown setting `{id}`"),
            Self::ReadOnlySetting { id } => write!(formatter, "setting `{id}` is read-only"),
            Self::ResetUnavailable { id } => {
                write!(formatter, "setting `{id}` does not support reset")
            }
            Self::InvalidValue { id, reason } => {
                write!(formatter, "invalid value for setting `{id}`: {reason}")
            }
            Self::AccessFailed { id, message } => {
                if let Some(id) = id {
                    write!(formatter, "access failed for setting `{id}`: {message}")
                } else {
                    write!(formatter, "setting access failed: {message}")
                }
            }
        }
    }
}

impl Error for SettingsError {}

/// Descriptor-level value validation error.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValueError {
    /// Descriptor value type and editor kind disagree.
    DescriptorTypeMismatch {
        /// Type expected by editor metadata.
        expected: SettingValueType,

        /// Type declared by descriptor.
        actual: SettingValueType,
    },

    /// Runtime value has a different kind than the descriptor expects.
    UnexpectedType {
        /// Expected value type.
        expected: SettingValueType,

        /// Actual value kind.
        actual: SettingValueKind,
    },

    /// Numeric value is outside the descriptor range.
    OutOfRange {
        /// Which side of the range failed.
        limit: ValueRangeLimit,

        /// Inclusive minimum.
        min: f64,

        /// Inclusive maximum.
        max: f64,

        /// Actual numeric value.
        actual: f64,
    },

    /// Static select value is not present in descriptor options.
    UnknownStaticOption { option_id: SettingOptionId },

    /// Numeric vector has the wrong length.
    InvalidVectorLength {
        /// Required vector length.
        expected: usize,

        /// Actual vector length.
        actual: usize,
    },

    /// Ordered select-list length violates descriptor constraints.
    InvalidListLength {
        /// Which side of the length range failed.
        limit: ListLengthLimitKind,

        /// Expected bound.
        expected: usize,

        /// Actual item count.
        actual: usize,
    },

    /// Ordered select-list contains the same option more than once.
    DuplicateListOption { option_id: SettingOptionId },

    /// Text length violates the descriptor constraints.
    InvalidTextLength {
        /// Which side of the length range failed.
        limit: TextLengthLimitKind,

        /// Expected bound.
        expected: usize,

        /// Actual character count.
        actual: usize,
    },
}

impl fmt::Display for SettingValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DescriptorTypeMismatch { expected, actual } => {
                write!(
                    formatter,
                    "descriptor type mismatch, editor expects `{expected}` but descriptor declares `{actual}`"
                )
            }
            Self::UnexpectedType { expected, actual } => {
                write!(formatter, "expected `{expected}`, got `{actual}`")
            }
            Self::OutOfRange {
                limit,
                min,
                max,
                actual,
            } => write!(
                formatter,
                "{limit} range violation: value {actual} is outside [{min}, {max}]"
            ),
            Self::UnknownStaticOption { option_id } => {
                write!(formatter, "unknown static option `{option_id}`")
            }
            Self::InvalidVectorLength { expected, actual } => {
                write!(formatter, "expected vector length {expected}, got {actual}")
            }
            Self::InvalidListLength {
                limit,
                expected,
                actual,
            } => write!(
                formatter,
                "{limit} list length violation: expected {expected}, got {actual}"
            ),
            Self::DuplicateListOption { option_id } => {
                write!(formatter, "duplicate list option `{option_id}`")
            }
            Self::InvalidTextLength {
                limit,
                expected,
                actual,
            } => write!(
                formatter,
                "{limit} text length violation: expected {expected}, got {actual}"
            ),
        }
    }
}

impl Error for SettingValueError {}

/// Side of a numeric range that failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueRangeLimit {
    /// Value is below minimum.
    Minimum,

    /// Value is above maximum.
    Maximum,
}

impl fmt::Display for ValueRangeLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Minimum => formatter.write_str("minimum"),
            Self::Maximum => formatter.write_str("maximum"),
        }
    }
}

/// Side of an ordered list length range that failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListLengthLimitKind {
    /// List is shorter than minimum.
    Minimum,

    /// List is longer than maximum.
    Maximum,
}

impl fmt::Display for ListLengthLimitKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Minimum => formatter.write_str("minimum"),
            Self::Maximum => formatter.write_str("maximum"),
        }
    }
}

/// Side of a text length range that failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextLengthLimitKind {
    /// Text is shorter than minimum.
    Minimum,

    /// Text is longer than maximum.
    Maximum,
}

impl fmt::Display for TextLengthLimitKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Minimum => formatter.write_str("minimum"),
            Self::Maximum => formatter.write_str("maximum"),
        }
    }
}
