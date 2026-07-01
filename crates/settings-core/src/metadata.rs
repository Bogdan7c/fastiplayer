use std::{collections::HashSet, fmt};

use crate::error::{ListLengthLimitKind, SettingValueError, TextLengthLimitKind, ValueRangeLimit};
use crate::options::{OptionProviderId, SettingOption, SettingOptionId};

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
    SettingId,
    "Stable runtime-facing setting id, for example `render.color.brightness`."
);
owned_string_id!(
    SettingPath,
    "Stable document path used for diagnostics, usually matching a TOML field path."
);
owned_string_id!(
    SettingTextId,
    "Stable localization key for user-facing setting text."
);
owned_string_id!(
    SettingsSurfaceId,
    "Stable id of the preferred visual surface that should host a setting."
);
owned_string_id!(SettingSectionId, "Stable id of a settings section.");
owned_string_id!(
    SettingGroupId,
    "Stable id of a settings group inside a section."
);
owned_string_id!(
    SettingRouteId,
    "Stable owner-level route id used by runtime apply/preview layers."
);
owned_string_id!(
    SettingUnit,
    "Stable unit label id for numeric editors, for example `percent` or `ms`."
);

/// Typed value passed across the neutral settings boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    /// Boolean value for toggles.
    Bool(bool),

    /// Integer numeric value.
    Integer(i64),

    /// Floating-point numeric value.
    Float(f64),

    /// Free-form text value.
    Text(String),

    /// Stable selected option id.
    Select(SettingOptionId),

    /// Ordered list of stable selected option ids.
    SelectList(Vec<SettingOptionId>),

    /// Fixed-length vector of numeric values, for example RGB coefficients.
    NumericVector(Vec<f64>),
}

impl SettingValue {
    /// Returns the lightweight kind used in validation diagnostics.
    #[must_use]
    pub const fn kind(&self) -> SettingValueKind {
        match self {
            Self::Bool(_) => SettingValueKind::Bool,
            Self::Integer(_) => SettingValueKind::Integer,
            Self::Float(_) => SettingValueKind::Float,
            Self::Text(_) => SettingValueKind::Text,
            Self::Select(_) => SettingValueKind::Select,
            Self::SelectList(_) => SettingValueKind::SelectList,
            Self::NumericVector(_) => SettingValueKind::NumericVector,
        }
    }
}

/// Lightweight kind of a runtime setting value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingValueKind {
    /// Boolean value.
    Bool,

    /// Integer numeric value.
    Integer,

    /// Floating-point numeric value.
    Float,

    /// Text value.
    Text,

    /// Select option id.
    Select,

    /// Ordered list of selected option ids.
    SelectList,

    /// Numeric vector.
    NumericVector,
}

impl fmt::Display for SettingValueKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Bool => "bool",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Text => "text",
            Self::Select => "select",
            Self::SelectList => "select_list",
            Self::NumericVector => "numeric_vector",
        };
        formatter.write_str(name)
    }
}

/// Declared value family for a setting descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingValueType {
    /// Boolean value.
    Bool,

    /// Integer numeric value.
    Integer,

    /// Floating-point numeric value.
    Float,

    /// Text value.
    Text,

    /// Stable selected option id.
    Select,

    /// Ordered list of stable selected option ids.
    SelectList,

    /// Fixed-length numeric vector.
    NumericVector,
}

impl fmt::Display for SettingValueType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Bool => "bool",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Text => "text",
            Self::Select => "select",
            Self::SelectList => "select_list",
            Self::NumericVector => "numeric_vector",
        };
        formatter.write_str(name)
    }
}

/// Stable text id plus Russian fallback visible in v1 UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingText {
    /// Stable localization key.
    pub text_id: SettingTextId,

    /// Russian fallback text used until a full localization catalog exists.
    pub fallback_ru: String,
}

impl SettingText {
    /// Creates text metadata with an owned id and fallback.
    #[must_use]
    pub fn new(text_id: impl Into<SettingTextId>, fallback_ru: impl Into<String>) -> Self {
        Self {
            text_id: text_id.into(),
            fallback_ru: fallback_ru.into(),
        }
    }
}

/// User-facing text bundle for one setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingDescriptorText {
    /// Required label.
    pub label: SettingText,

    /// Short description for settings lists and diagnostics.
    pub description: Option<SettingText>,

    /// Longer help text for contextual UI.
    pub help: Option<SettingText>,
}

impl SettingDescriptorText {
    /// Creates a descriptor text bundle with a required label.
    #[must_use]
    pub fn new(label: SettingText) -> Self {
        Self {
            label,
            description: None,
            help: None,
        }
    }

    /// Adds an optional description.
    #[must_use]
    pub fn with_description(mut self, description: SettingText) -> Self {
        self.description = Some(description);
        self
    }

    /// Adds optional help text.
    #[must_use]
    pub fn with_help(mut self, help: SettingText) -> Self {
        self.help = Some(help);
        self
    }
}

/// Stable placement metadata that keeps visual layout out of storage accessors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingPlacement {
    /// Top-level section id.
    pub section: SettingSectionId,

    /// Section-local group id.
    pub group: SettingGroupId,

    /// Preferred visual surface for this setting.
    pub preferred_surface: SettingsSurfaceId,

    /// Initial open-state hint for this field's group on a fresh UI session.
    pub group_default_open: bool,
}

impl SettingPlacement {
    /// Creates placement metadata from stable ids.
    #[must_use]
    pub fn new(
        section: impl Into<SettingSectionId>,
        group: impl Into<SettingGroupId>,
        preferred_surface: impl Into<SettingsSurfaceId>,
    ) -> Self {
        Self {
            section: section.into(),
            group: group.into(),
            preferred_surface: preferred_surface.into(),
            group_default_open: true,
        }
    }

    /// Overrides the initial open-state hint for this field's visual group.
    #[must_use]
    pub const fn with_group_default_open(mut self, group_default_open: bool) -> Self {
        self.group_default_open = group_default_open;
        self
    }
}

/// Declares whether user edits may write through this setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingAccess {
    /// Normal editable setting.
    ReadWrite,

    /// Visible but protected setting, for example schema version.
    ReadOnly,
}

impl SettingAccess {
    /// Returns whether a set/reset operation may change the document.
    #[must_use]
    pub const fn permits_write(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

/// Default/reset policy for a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultBehavior {
    /// Reset reads the value from the default document through the accessor.
    FromDefaultDocument,

    /// Reset is intentionally unavailable.
    NoReset,
}

/// Runtime apply semantics declared by metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingApplyMode {
    /// Value is applied only by committed Apply/OK flow.
    CommittedApply,

    /// Draft changes may be previewed by the runtime owner before Apply.
    ImmediatePreview,
}

/// Complete project-agnostic descriptor for one setting.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingDescriptor {
    /// Stable setting id.
    pub id: SettingId,

    /// Stable document path used for diagnostics.
    pub path: SettingPath,

    /// Text metadata with localization ids and Russian fallbacks.
    pub text: SettingDescriptorText,

    /// Visual placement metadata.
    pub placement: SettingPlacement,

    /// Declared value family.
    pub value_type: SettingValueType,

    /// Editor metadata used by visual layers.
    pub editor: SettingEditor,

    /// Read/write policy.
    pub access: SettingAccess,

    /// Reset policy.
    pub default_behavior: DefaultBehavior,

    /// Owner-level route id.
    pub route: SettingRouteId,

    /// Apply/preview mode for the route.
    pub apply_mode: SettingApplyMode,
}

impl SettingDescriptor {
    /// Validates a neutral value against this descriptor.
    pub fn validate_value(&self, value: &SettingValue) -> Result<(), SettingValueError> {
        self.editor.validate_value(self.value_type, value)
    }
}

/// Neutral editor metadata for a setting.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingEditor {
    /// Boolean toggle.
    Toggle,

    /// Numeric editor.
    Numeric(NumericDescriptor),

    /// Select/dropdown editor.
    Select(SelectDescriptor),

    /// Ordered list editor over stable option ids.
    SelectList(SelectListDescriptor),

    /// Text input editor.
    Text(TextDescriptor),

    /// `auto` or a positive fixed integer stored as text.
    AutoFixedPositiveInteger(AutoFixedPositiveIntegerDescriptor),

    /// Fixed-length numeric vector editor.
    Vector(VectorDescriptor),

    /// Read-only display.
    ReadOnly,
}

impl SettingEditor {
    /// Validates the value shape and simple field constraints declared by metadata.
    pub fn validate_value(
        &self,
        value_type: SettingValueType,
        value: &SettingValue,
    ) -> Result<(), SettingValueError> {
        match self {
            Self::Toggle => validate_value_type(value_type, SettingValueType::Bool, value),
            Self::Numeric(descriptor) => descriptor.validate_value(value_type, value),
            Self::Select(descriptor) => descriptor.validate_value(value_type, value),
            Self::SelectList(descriptor) => descriptor.validate_value(value_type, value),
            Self::Text(descriptor) => descriptor.validate_value(value_type, value),
            Self::AutoFixedPositiveInteger(descriptor) => {
                descriptor.validate_value(value_type, value)
            }
            Self::Vector(descriptor) => descriptor.validate_value(value_type, value),
            Self::ReadOnly => validate_read_only_value_type(value_type, value),
        }
    }
}

/// Numeric editor metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct NumericDescriptor {
    /// Allowed numeric range.
    pub range: NumericRange,

    /// UI step hint.
    pub step: NumericStep,

    /// Optional unit id.
    pub unit: Option<SettingUnit>,
}

impl NumericDescriptor {
    /// Creates numeric metadata with a range and step.
    #[must_use]
    pub const fn new(range: NumericRange, step: NumericStep, unit: Option<SettingUnit>) -> Self {
        Self { range, step, unit }
    }

    fn validate_value(
        &self,
        value_type: SettingValueType,
        value: &SettingValue,
    ) -> Result<(), SettingValueError> {
        match (&self.range, value) {
            (NumericRange::Integer { min, max }, SettingValue::Integer(number)) => {
                validate_value_type(value_type, SettingValueType::Integer, value)?;
                if number < min {
                    return Err(SettingValueError::OutOfRange {
                        limit: ValueRangeLimit::Minimum,
                        min: *min as f64,
                        max: *max as f64,
                        actual: *number as f64,
                    });
                }
                if number > max {
                    return Err(SettingValueError::OutOfRange {
                        limit: ValueRangeLimit::Maximum,
                        min: *min as f64,
                        max: *max as f64,
                        actual: *number as f64,
                    });
                }
                Ok(())
            }
            (NumericRange::Float { min, max }, SettingValue::Float(number)) => {
                validate_value_type(value_type, SettingValueType::Float, value)?;
                if number < min {
                    return Err(SettingValueError::OutOfRange {
                        limit: ValueRangeLimit::Minimum,
                        min: *min,
                        max: *max,
                        actual: *number,
                    });
                }
                if number > max {
                    return Err(SettingValueError::OutOfRange {
                        limit: ValueRangeLimit::Maximum,
                        min: *min,
                        max: *max,
                        actual: *number,
                    });
                }
                Ok(())
            }
            (NumericRange::Integer { .. }, _) => Err(SettingValueError::UnexpectedType {
                expected: SettingValueType::Integer,
                actual: value.kind(),
            }),
            (NumericRange::Float { .. }, _) => Err(SettingValueError::UnexpectedType {
                expected: SettingValueType::Float,
                actual: value.kind(),
            }),
        }
    }
}

/// Allowed numeric range.
#[derive(Debug, Clone, PartialEq)]
pub enum NumericRange {
    /// Inclusive integer range.
    Integer { min: i64, max: i64 },

    /// Inclusive floating-point range.
    Float { min: f64, max: f64 },
}

/// UI step hint for numeric editors.
#[derive(Debug, Clone, PartialEq)]
pub enum NumericStep {
    /// Integer step.
    Integer(i64),

    /// Floating-point step.
    Float(f64),
}

/// Select editor metadata.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectDescriptor {
    /// Options are fully known from metadata.
    Static { options: Vec<SettingOption> },

    /// Options are provided and cached by a runtime owner.
    Dynamic { provider_id: OptionProviderId },
}

impl SelectDescriptor {
    fn validate_value(
        &self,
        value_type: SettingValueType,
        value: &SettingValue,
    ) -> Result<(), SettingValueError> {
        validate_value_type(value_type, SettingValueType::Select, value)?;
        let SettingValue::Select(selected_id) = value else {
            return Err(SettingValueError::UnexpectedType {
                expected: SettingValueType::Select,
                actual: value.kind(),
            });
        };

        match self {
            Self::Static { options } => {
                if options.iter().any(|option| option.id == *selected_id) {
                    Ok(())
                } else {
                    Err(SettingValueError::UnknownStaticOption {
                        option_id: selected_id.clone(),
                    })
                }
            }
            Self::Dynamic { .. } => Ok(()),
        }
    }
}

/// Ordered static select-list editor metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectListDescriptor {
    /// Options that may appear in the ordered list.
    pub options: Vec<SettingOption>,

    /// Minimum number of selected options.
    pub min_len: usize,

    /// Optional maximum number of selected options.
    pub max_len: Option<usize>,

    /// Whether one option id may appear more than once.
    pub allow_duplicates: bool,
}

impl SelectListDescriptor {
    /// Creates a unique ordered-list descriptor with no length bounds.
    #[must_use]
    pub fn new(options: Vec<SettingOption>) -> Self {
        Self {
            options,
            min_len: 0,
            max_len: None,
            allow_duplicates: false,
        }
    }

    /// Adds a minimum list length.
    #[must_use]
    pub const fn with_min_len(mut self, min_len: usize) -> Self {
        self.min_len = min_len;
        self
    }

    /// Adds a maximum list length.
    #[must_use]
    pub const fn with_max_len(mut self, max_len: usize) -> Self {
        self.max_len = Some(max_len);
        self
    }

    /// Allows repeated option ids when a future setting explicitly needs them.
    #[must_use]
    pub const fn with_duplicates_allowed(mut self) -> Self {
        self.allow_duplicates = true;
        self
    }

    fn validate_value(
        &self,
        value_type: SettingValueType,
        value: &SettingValue,
    ) -> Result<(), SettingValueError> {
        validate_value_type(value_type, SettingValueType::SelectList, value)?;
        let SettingValue::SelectList(selected_ids) = value else {
            return Err(SettingValueError::UnexpectedType {
                expected: SettingValueType::SelectList,
                actual: value.kind(),
            });
        };

        if selected_ids.len() < self.min_len {
            return Err(SettingValueError::InvalidListLength {
                limit: ListLengthLimitKind::Minimum,
                expected: self.min_len,
                actual: selected_ids.len(),
            });
        }

        if let Some(max_len) = self.max_len {
            if selected_ids.len() > max_len {
                return Err(SettingValueError::InvalidListLength {
                    limit: ListLengthLimitKind::Maximum,
                    expected: max_len,
                    actual: selected_ids.len(),
                });
            }
        }

        let mut seen_ids = HashSet::new();
        for selected_id in selected_ids {
            if self.options.iter().all(|option| option.id != *selected_id) {
                return Err(SettingValueError::UnknownStaticOption {
                    option_id: selected_id.clone(),
                });
            }

            if !self.allow_duplicates && !seen_ids.insert(selected_id) {
                return Err(SettingValueError::DuplicateListOption {
                    option_id: selected_id.clone(),
                });
            }
        }

        Ok(())
    }
}

/// Editor metadata for values stored as `auto` or an explicit positive integer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoFixedPositiveIntegerDescriptor {
    /// Label for automatic resolver mode.
    pub auto_label: SettingText,

    /// Label for explicit fixed mode.
    pub fixed_label: SettingText,

    /// Optional unit displayed near the fixed integer.
    pub unit: Option<SettingUnit>,
}

impl AutoFixedPositiveIntegerDescriptor {
    /// Creates Auto/Fixed-positive editor metadata.
    #[must_use]
    pub fn new(
        auto_label: SettingText,
        fixed_label: SettingText,
        unit: Option<SettingUnit>,
    ) -> Self {
        Self {
            auto_label,
            fixed_label,
            unit,
        }
    }

    fn validate_value(
        &self,
        value_type: SettingValueType,
        value: &SettingValue,
    ) -> Result<(), SettingValueError> {
        validate_value_type(value_type, SettingValueType::Text, value)?;
        let SettingValue::Text(text) = value else {
            return Err(SettingValueError::UnexpectedType {
                expected: SettingValueType::Text,
                actual: value.kind(),
            });
        };

        if auto_fixed_positive_integer_text_is_valid(text) {
            Ok(())
        } else {
            Err(SettingValueError::InvalidAutoFixedPositiveInteger {
                actual: text.clone(),
            })
        }
    }
}

fn auto_fixed_positive_integer_text_is_valid(text: &str) -> bool {
    let trimmed_text = text.trim();
    if trimmed_text == "auto" {
        return true;
    }

    trimmed_text
        .parse::<usize>()
        .is_ok_and(|fixed_value| fixed_value > 0)
}

/// Text editor metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextDescriptor {
    /// Single-line or multiline input.
    pub format: TextFormat,

    /// Optional minimum character length.
    pub min_len: Option<usize>,

    /// Optional maximum character length.
    pub max_len: Option<usize>,
}

impl TextDescriptor {
    /// Creates text metadata without length limits.
    #[must_use]
    pub const fn new(format: TextFormat) -> Self {
        Self {
            format,
            min_len: None,
            max_len: None,
        }
    }

    /// Adds a minimum length constraint.
    #[must_use]
    pub const fn with_min_len(mut self, min_len: usize) -> Self {
        self.min_len = Some(min_len);
        self
    }

    /// Adds a maximum length constraint.
    #[must_use]
    pub const fn with_max_len(mut self, max_len: usize) -> Self {
        self.max_len = Some(max_len);
        self
    }

    fn validate_value(
        &self,
        value_type: SettingValueType,
        value: &SettingValue,
    ) -> Result<(), SettingValueError> {
        validate_value_type(value_type, SettingValueType::Text, value)?;
        let SettingValue::Text(text) = value else {
            return Err(SettingValueError::UnexpectedType {
                expected: SettingValueType::Text,
                actual: value.kind(),
            });
        };

        let length = text.chars().count();
        if let Some(min_len) = self.min_len {
            if length < min_len {
                return Err(SettingValueError::InvalidTextLength {
                    limit: TextLengthLimitKind::Minimum,
                    expected: min_len,
                    actual: length,
                });
            }
        }
        if let Some(max_len) = self.max_len {
            if length > max_len {
                return Err(SettingValueError::InvalidTextLength {
                    limit: TextLengthLimitKind::Maximum,
                    expected: max_len,
                    actual: length,
                });
            }
        }

        Ok(())
    }
}

/// Text input format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFormat {
    /// One-line text input.
    SingleLine,

    /// Multiline text input.
    Multiline,
}

/// Fixed-length numeric vector editor metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorDescriptor {
    /// Numeric descriptor shared by each element.
    pub element: NumericDescriptor,

    /// Human-readable labels for each vector element.
    pub labels: Vec<SettingText>,

    /// Required vector length.
    pub expected_len: usize,
}

impl VectorDescriptor {
    /// Creates vector metadata.
    #[must_use]
    pub fn new(element: NumericDescriptor, labels: Vec<SettingText>, expected_len: usize) -> Self {
        Self {
            element,
            labels,
            expected_len,
        }
    }

    fn validate_value(
        &self,
        value_type: SettingValueType,
        value: &SettingValue,
    ) -> Result<(), SettingValueError> {
        validate_value_type(value_type, SettingValueType::NumericVector, value)?;
        let SettingValue::NumericVector(numbers) = value else {
            return Err(SettingValueError::UnexpectedType {
                expected: SettingValueType::NumericVector,
                actual: value.kind(),
            });
        };

        if numbers.len() != self.expected_len {
            return Err(SettingValueError::InvalidVectorLength {
                expected: self.expected_len,
                actual: numbers.len(),
            });
        }

        for number in numbers {
            self.element
                .validate_value(SettingValueType::Float, &SettingValue::Float(*number))?;
        }

        Ok(())
    }
}

fn validate_value_type(
    actual_descriptor_type: SettingValueType,
    expected_descriptor_type: SettingValueType,
    value: &SettingValue,
) -> Result<(), SettingValueError> {
    if actual_descriptor_type != expected_descriptor_type {
        return Err(SettingValueError::DescriptorTypeMismatch {
            expected: expected_descriptor_type,
            actual: actual_descriptor_type,
        });
    }

    let actual_value_type = match value.kind() {
        SettingValueKind::Bool => SettingValueType::Bool,
        SettingValueKind::Integer => SettingValueType::Integer,
        SettingValueKind::Float => SettingValueType::Float,
        SettingValueKind::Text => SettingValueType::Text,
        SettingValueKind::Select => SettingValueType::Select,
        SettingValueKind::SelectList => SettingValueType::SelectList,
        SettingValueKind::NumericVector => SettingValueType::NumericVector,
    };

    if actual_value_type == expected_descriptor_type {
        Ok(())
    } else {
        Err(SettingValueError::UnexpectedType {
            expected: expected_descriptor_type,
            actual: value.kind(),
        })
    }
}

fn validate_read_only_value_type(
    expected_descriptor_type: SettingValueType,
    value: &SettingValue,
) -> Result<(), SettingValueError> {
    let actual_value_type = match value.kind() {
        SettingValueKind::Bool => SettingValueType::Bool,
        SettingValueKind::Integer => SettingValueType::Integer,
        SettingValueKind::Float => SettingValueType::Float,
        SettingValueKind::Text => SettingValueType::Text,
        SettingValueKind::Select => SettingValueType::Select,
        SettingValueKind::SelectList => SettingValueType::SelectList,
        SettingValueKind::NumericVector => SettingValueType::NumericVector,
    };

    if actual_value_type == expected_descriptor_type {
        Ok(())
    } else {
        Err(SettingValueError::UnexpectedType {
            expected: expected_descriptor_type,
            actual: value.kind(),
        })
    }
}
