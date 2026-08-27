//! Bounded XML name/attribute/string/numeric decoding for the DASH schema.

use bounded_xml_reader::{XmlElement, XmlEvent, XmlExpandedName};

use crate::error::{DashMpdError, DashMpdErrorKind};
use crate::model::DASH_MPD_NAMESPACE;

use super::{DashMpdLimits, EventCursor};

/// Читает text-only leaf, не превышая общий schema string budget.
pub(super) fn read_text_leaf(
    cursor: &mut EventCursor<'_>,
    expected_end: &str,
    limits: DashMpdLimits,
) -> Result<String, DashMpdError> {
    let mut text = String::new();
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::Text(chunk)) => {
                if text.len().saturating_add(chunk.content().len())
                    > limits.maximum_schema_string_bytes
                {
                    return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded));
                }
                text.push_str(chunk.content());
            }
            Some(XmlEvent::EndElement(name)) if is_name(&name, expected_end) => break,
            Some(_) | None => {
                return Err(DashMpdError::new(DashMpdErrorKind::MalformedSchema));
            }
        }
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(DashMpdError::new(DashMpdErrorKind::InvalidAttribute));
    }
    Ok(trimmed.to_owned())
}

/// Проверяет expanded name и exact DASH namespace.
pub(crate) fn require_name(
    name: &XmlExpandedName,
    local_name: &str,
    kind: DashMpdErrorKind,
) -> Result<(), DashMpdError> {
    if !is_name(name, local_name) {
        return Err(DashMpdError::new(kind));
    }
    Ok(())
}

/// Exact namespace/local-name predicate.
pub(crate) fn is_name(name: &XmlExpandedName, local_name: &str) -> bool {
    name.namespace_uri() == Some(DASH_MPD_NAMESPACE) && name.local_name() == local_name
}

/// Разрешает только перечисленные unqualified attributes и запрещает xlink.
pub(crate) fn validate_attributes(
    element: &XmlElement,
    allowed: &[&str],
) -> Result<(), DashMpdError> {
    for attribute in element.attributes() {
        if attribute.name().namespace_uri().is_some()
            || !allowed.contains(&attribute.name().local_name())
        {
            return Err(DashMpdError::new(DashMpdErrorKind::UnsupportedConstruct));
        }
    }
    Ok(())
}

/// Находит unqualified attribute.
pub(crate) fn optional_attribute<'element>(
    element: &'element XmlElement,
    name: &str,
) -> Result<Option<&'element str>, DashMpdError> {
    let mut found = None;
    for attribute in element.attributes() {
        if attribute.name().namespace_uri().is_none()
            && attribute.name().local_name() == name
            && found.replace(attribute.value()).is_some()
        {
            return Err(DashMpdError::new(DashMpdErrorKind::InvalidAttribute));
        }
    }
    Ok(found)
}

/// Читает bounded optional string.
pub(crate) fn bounded_optional_attribute(
    element: &XmlElement,
    name: &str,
    limits: DashMpdLimits,
) -> Result<Option<String>, DashMpdError> {
    optional_attribute(element, name)?
        .map(|value| bounded_string(value, limits))
        .transpose()
}

/// Читает bounded required string.
pub(super) fn required_bounded_attribute(
    element: &XmlElement,
    name: &str,
    limits: DashMpdLimits,
) -> Result<String, DashMpdError> {
    let value = optional_attribute(element, name)?
        .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?;
    bounded_string(value, limits)
}

/// Применяет единый string cap до allocation.
fn bounded_string(value: &str, limits: DashMpdLimits) -> Result<String, DashMpdError> {
    if value.is_empty() || value.len() > limits.maximum_schema_string_bytes {
        return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded));
    }
    Ok(value.to_owned())
}

/// Читает optional unsigned integer.
pub(super) fn optional_u64_attribute(
    element: &XmlElement,
    name: &str,
) -> Result<Option<u64>, DashMpdError> {
    optional_attribute(element, name)?
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))
        })
        .transpose()
}

/// Читает optional positive dimension без silent truncation.
pub(super) fn optional_positive_u32_attribute(
    element: &XmlElement,
    name: &str,
) -> Result<Option<u32>, DashMpdError> {
    optional_attribute(element, name)?
        .map(|value| {
            value
                .parse::<u32>()
                .ok()
                .filter(|dimension| *dimension > 0)
                .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))
        })
        .transpose()
}

/// Читает bounded positive ratio с exact caller-owned separator-ом.
pub(super) fn optional_positive_ratio_attribute(
    element: &XmlElement,
    name: &str,
    separator: char,
    limits: DashMpdLimits,
) -> Result<Option<(u32, u32)>, DashMpdError> {
    bounded_optional_attribute(element, name, limits)?
        .map(|value| {
            let (numerator, denominator) = value
                .split_once(separator)
                .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?;
            let numerator = numerator
                .parse::<u32>()
                .ok()
                .filter(|part| *part > 0)
                .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?;
            let denominator = denominator
                .parse::<u32>()
                .ok()
                .filter(|part| *part > 0)
                .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?;
            Ok((numerator, denominator))
        })
        .transpose()
}
