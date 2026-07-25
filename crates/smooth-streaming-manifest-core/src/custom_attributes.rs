//! Immutable bounded standard `CustomAttributes` model.

use std::fmt;

use crate::error::{SmoothManifestError, SmoothSchemaField};
use crate::limits::{SmoothManifestLimitKind, SmoothManifestLimits};

/// Safe grammar имени standard CustomAttribute.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SmoothCustomAttributeName(Box<str>);

impl SmoothCustomAttributeName {
    /// Создаётся только parser-ом после XML schema admission.
    pub(crate) fn new(
        name: impl Into<String>,
        limits: &SmoothManifestLimits,
    ) -> Result<Self, SmoothManifestError> {
        let name = name.into();
        validate_template_atom(
            &name,
            limits.maximum_custom_attribute_name_bytes(),
            SmoothManifestLimitKind::CustomAttributeNameBytes,
        )?;
        Ok(Self(name.into_boxed_str()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SmoothCustomAttributeName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothCustomAttributeName")
            .field("bytes", &self.0.len())
            .finish()
    }
}

/// Safe grammar значения standard CustomAttribute.
#[derive(Clone, PartialEq, Eq)]
pub struct SmoothCustomAttributeValue(Box<str>);

impl SmoothCustomAttributeValue {
    /// Создаётся только parser-ом после XML schema admission.
    pub(crate) fn new(
        value: impl Into<String>,
        limits: &SmoothManifestLimits,
    ) -> Result<Self, SmoothManifestError> {
        let value = value.into();
        validate_template_atom(
            &value,
            limits.maximum_custom_attribute_value_bytes(),
            SmoothManifestLimitKind::CustomAttributeValueBytes,
        )?;
        Ok(Self(value.into_boxed_str()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SmoothCustomAttributeValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothCustomAttributeValue")
            .field("bytes", &self.0.len())
            .finish()
    }
}

/// Одна bounded name/value пара standard CustomAttributes.
#[derive(Clone, PartialEq, Eq)]
pub struct SmoothCustomAttribute {
    name: SmoothCustomAttributeName,
    value: SmoothCustomAttributeValue,
}

impl SmoothCustomAttribute {
    /// Parser-only constructor не позволяет downstream обходить manifest proof.
    #[must_use]
    pub(crate) const fn new(
        name: SmoothCustomAttributeName,
        value: SmoothCustomAttributeValue,
    ) -> Self {
        Self { name, value }
    }

    #[must_use]
    pub const fn name(&self) -> &SmoothCustomAttributeName {
        &self.name
    }

    #[must_use]
    pub const fn value(&self) -> &SmoothCustomAttributeValue {
        &self.value
    }
}

impl fmt::Debug for SmoothCustomAttribute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothCustomAttribute")
            .field("name", &self.name)
            .field("value", &self.value)
            .finish()
    }
}

/// Immutable ordered set исключает ambiguous duplicate names.
#[derive(Clone, PartialEq, Eq)]
pub struct SmoothCustomAttributeSet(Box<[SmoothCustomAttribute]>);

impl SmoothCustomAttributeSet {
    /// Parser-only construction применяет count budget до публикации set-а.
    pub(crate) fn new(
        attributes: Vec<SmoothCustomAttribute>,
        limits: &SmoothManifestLimits,
    ) -> Result<Self, SmoothManifestError> {
        if attributes.len() > limits.maximum_custom_attributes_per_quality() {
            return Err(SmoothManifestError::LimitExceeded {
                limit: SmoothManifestLimitKind::CustomAttributesPerQuality,
                maximum: limits.maximum_custom_attributes_per_quality(),
            });
        }
        for (index, attribute) in attributes.iter().enumerate() {
            if attributes[..index]
                .iter()
                .any(|existing| existing.name == attribute.name)
            {
                return Err(SmoothManifestError::MalformedSchema {
                    field: SmoothSchemaField::QualityLevel,
                });
            }
        }
        Ok(Self(attributes.into_boxed_slice()))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn as_slice(&self) -> &[SmoothCustomAttribute] {
        &self.0
    }

    /// Рендерит только уже проверенные atoms в template-owned output.
    pub(crate) fn append_template_component(&self, output: &mut String) {
        for (index, attribute) in self.0.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            output.push_str(attribute.name.as_str());
            output.push('=');
            output.push_str(attribute.value.as_str());
        }
    }
}

impl fmt::Debug for SmoothCustomAttributeSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothCustomAttributeSet")
            .field("count", &self.0.len())
            .finish()
    }
}

/// Template atoms исключают separators, traversal и control characters.
fn validate_template_atom(
    value: &str,
    maximum: usize,
    limit: SmoothManifestLimitKind,
) -> Result<(), SmoothManifestError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(SmoothManifestError::MalformedSchema {
            field: SmoothSchemaField::QualityLevel,
        });
    }
    if value.len() > maximum {
        return Err(SmoothManifestError::LimitExceeded { limit, maximum });
    }
    Ok(())
}
