use std::error::Error;
use std::fmt;

use crate::metadata::{SettingId, SettingText, SettingValue};

macro_rules! owned_string_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Creates an owned stable identifier.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the stable identifier as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consumes the wrapper and returns the owned identifier.
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

owned_string_id!(
    SettingOptionId,
    "Stable id stored for a select option, never a display label."
);
owned_string_id!(
    OptionProviderId,
    "Stable id of a runtime-owned dynamic option provider."
);

/// Select option metadata returned by static descriptors or dynamic providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingOption {
    /// Stable id stored in config/runtime values.
    pub id: SettingOptionId,

    /// User-facing option label.
    pub label: SettingText,

    /// Optional user-facing option description.
    pub description: Option<SettingText>,
}

impl SettingOption {
    /// Creates an option with stable id and label.
    #[must_use]
    pub fn new(id: impl Into<SettingOptionId>, label: SettingText) -> Self {
        Self {
            id: id.into(),
            label,
            description: None,
        }
    }

    /// Adds an optional description.
    #[must_use]
    pub fn with_description(mut self, description: SettingText) -> Self {
        self.description = Some(description);
        self
    }
}

/// Request passed to a dynamic option provider by the runtime owner.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingOptionsRequest {
    /// Setting that requested the options.
    pub setting_id: SettingId,

    /// Current draft or committed value, if the caller has one.
    pub current_value: Option<SettingValue>,
}

impl SettingOptionsRequest {
    /// Creates an options request.
    #[must_use]
    pub fn new(setting_id: impl Into<SettingId>, current_value: Option<SettingValue>) -> Self {
        Self {
            setting_id: setting_id.into(),
            current_value,
        }
    }
}

/// Dynamic option cache/provider state as seen by neutral UI code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingOptionsStatus {
    /// Runtime owner уже знает provider, но snapshot ещё не загружен.
    Loading,

    /// Options are fresh enough for current UI.
    Ready,

    /// Options are available but should be refreshed when the runtime owner can.
    Stale,

    /// Provider cannot currently produce options.
    Unavailable { message: String },
}

/// Explicit representation of the current dynamic select value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingOptionCurrentValue {
    /// There is no current value.
    None,

    /// Current value is present in the available options list.
    Available(SettingOptionId),

    /// Current value is saved but absent from the current provider snapshot.
    UnavailableCurrent {
        /// Stable id that remains valid for persistence.
        id: SettingOptionId,

        /// Text shown for the unavailable saved value.
        label: SettingText,
    },
}

impl SettingOptionCurrentValue {
    /// Returns whether this current value is explicitly unavailable right now.
    #[must_use]
    pub const fn is_unavailable_current(&self) -> bool {
        matches!(self, Self::UnavailableCurrent { .. })
    }
}

/// Provider result consumed by runtime/cache/UI layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingOptions {
    /// Provider that produced this snapshot.
    pub provider_id: OptionProviderId,

    /// Snapshot status.
    pub status: SettingOptionsStatus,

    /// Available selectable options.
    pub options: Vec<SettingOption>,

    /// Current value, including an unavailable-current state when needed.
    pub current: SettingOptionCurrentValue,
}

impl SettingOptions {
    /// Creates a ready options snapshot.
    #[must_use]
    pub fn ready(
        provider_id: impl Into<OptionProviderId>,
        options: Vec<SettingOption>,
        current: SettingOptionCurrentValue,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            status: SettingOptionsStatus::Ready,
            options,
            current,
        }
    }
}

/// Dynamic option provider error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingOptionsError {
    /// Provider is absent or intentionally disabled.
    ProviderUnavailable { provider_id: OptionProviderId },

    /// Provider failed while collecting options.
    Failed {
        /// Provider that failed.
        provider_id: OptionProviderId,

        /// Human-readable diagnostic context.
        message: String,
    },
}

impl fmt::Display for SettingOptionsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProviderUnavailable { provider_id } => {
                write!(formatter, "option provider `{provider_id}` is unavailable")
            }
            Self::Failed {
                provider_id,
                message,
            } => write!(
                formatter,
                "option provider `{provider_id}` failed: {message}"
            ),
        }
    }
}

impl Error for SettingOptionsError {}

/// Runtime-owned contract for dynamic option collection.
pub trait SettingOptionProvider: Send + Sync {
    /// Returns the stable provider id.
    fn provider_id(&self) -> OptionProviderId;

    /// Collects options for a setting without exposing provider internals.
    fn options(
        &self,
        request: SettingOptionsRequest,
    ) -> Result<SettingOptions, SettingOptionsError>;
}
