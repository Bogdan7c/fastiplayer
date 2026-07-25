//! Строгая lexical и expanded-name policy Smooth Streaming schema.

use bounded_xml_reader::{XmlElement, XmlExpandedName};

use crate::error::{SmoothManifestError, SmoothSchemaField, SmoothUnsupportedConstruct};
use crate::limits::{SmoothManifestLimitKind, SmoothManifestLimits};

/// Проверяет exact unqualified element name.
pub(crate) fn is_unqualified_name(name: &XmlExpandedName, local_name: &str) -> bool {
    name.namespace_uri().is_none() && name.local_name() == local_name
}

/// Любой namespace является private extension, а не alias стандартной vocabulary.
pub(crate) fn require_unqualified_name(
    name: &XmlExpandedName,
    local_name: &str,
    field: SmoothSchemaField,
) -> Result<(), SmoothManifestError> {
    if name.namespace_uri().is_some() {
        return Err(SmoothManifestError::PrivateExtension);
    }
    if name.local_name() != local_name {
        return Err(SmoothManifestError::MalformedSchema { field });
    }
    Ok(())
}

/// Разрешает только exact список unqualified attributes.
pub(crate) fn validate_attributes(
    element: &XmlElement,
    allowed: &[&str],
) -> Result<(), SmoothManifestError> {
    for attribute in element.attributes() {
        if attribute.name().namespace_uri().is_some() {
            return Err(SmoothManifestError::PrivateExtension);
        }
        if !allowed.contains(&attribute.name().local_name()) {
            return Err(SmoothManifestError::UnsupportedConstruct {
                construct: SmoothUnsupportedConstruct::UnknownAttribute,
            });
        }
    }
    Ok(())
}

/// Возвращает один optional unqualified attribute; дубликат fail-closed.
pub(crate) fn optional_attribute<'element>(
    element: &'element XmlElement,
    name: &str,
) -> Result<Option<&'element str>, SmoothManifestError> {
    let mut found = None;
    for attribute in element.attributes() {
        if attribute.name().namespace_uri().is_none()
            && attribute.name().local_name() == name
            && found.replace(attribute.value()).is_some()
        {
            return Err(SmoothManifestError::MalformedSchema {
                field: SmoothSchemaField::Root,
            });
        }
    }
    Ok(found)
}

/// Возвращает обязательный attribute без потери field identity.
pub(crate) fn required_attribute<'element>(
    element: &'element XmlElement,
    name: &str,
    field: SmoothSchemaField,
) -> Result<&'element str, SmoothManifestError> {
    optional_attribute(element, name)?.ok_or(SmoothManifestError::MalformedSchema { field })
}

/// Проверяет caller-owned schema string budget без allocation.
pub(crate) fn validate_schema_string(
    value: &str,
    limits: &SmoothManifestLimits,
    field: SmoothSchemaField,
) -> Result<(), SmoothManifestError> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(SmoothManifestError::MalformedSchema { field });
    }
    if value.len() > limits.maximum_string_bytes() {
        return Err(SmoothManifestError::LimitExceeded {
            limit: SmoothManifestLimitKind::StringBytes,
            maximum: limits.maximum_string_bytes(),
        });
    }
    Ok(())
}

/// Парсит только ASCII decimal digits во всём диапазоне `u64`.
pub(crate) fn parse_u64(value: &str, field: SmoothSchemaField) -> Result<u64, SmoothManifestError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SmoothManifestError::MalformedSchema { field });
    }
    value
        .parse::<u64>()
        .map_err(|_| SmoothManifestError::MalformedSchema { field })
}

/// Парсит обязательное положительное `u64`.
pub(crate) fn parse_positive_u64(
    value: &str,
    field: SmoothSchemaField,
) -> Result<u64, SmoothManifestError> {
    let parsed = parse_u64(value, field)?;
    if parsed == 0 {
        return Err(SmoothManifestError::MalformedSchema { field });
    }
    Ok(parsed)
}

/// Делает checked narrowing только после полного `u64` parse.
pub(crate) fn parse_positive_u32(
    value: &str,
    field: SmoothSchemaField,
) -> Result<u32, SmoothManifestError> {
    u32::try_from(parse_positive_u64(value, field)?)
        .map_err(|_| SmoothManifestError::MalformedSchema { field })
}

/// Делает checked narrowing только после полного `u64` parse.
pub(crate) fn parse_positive_u16(
    value: &str,
    field: SmoothSchemaField,
) -> Result<u16, SmoothManifestError> {
    u16::try_from(parse_positive_u64(value, field)?)
        .map_err(|_| SmoothManifestError::MalformedSchema { field })
}

/// Принимает только standard Smooth Streaming boolean spellings.
pub(crate) fn parse_bool(
    value: &str,
    field: SmoothSchemaField,
) -> Result<bool, SmoothManifestError> {
    match value {
        "true" | "TRUE" => Ok(true),
        "false" | "FALSE" => Ok(false),
        _ => Err(SmoothManifestError::MalformedSchema { field }),
    }
}

/// Любой неизвестный child различает private namespace и unqualified vocabulary.
pub(crate) fn unsupported_child(name: &XmlExpandedName) -> SmoothManifestError {
    if name.namespace_uri().is_some() {
        SmoothManifestError::PrivateExtension
    } else {
        SmoothManifestError::UnsupportedConstruct {
            construct: SmoothUnsupportedConstruct::UnknownElement,
        }
    }
}
