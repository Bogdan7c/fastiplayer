use std::collections::HashSet;

use crate::{HlsLineNumber, HlsParseError, HlsParseErrorKind, HlsParserLimits};

/// Разобранный validated attribute-list, сохраняющий exact values.
pub(crate) struct Attributes<'a> {
    entries: Vec<(&'a str, &'a str)>,
}

impl<'a> Attributes<'a> {
    pub(crate) fn parse(
        raw: &'a str,
        line: HlsLineNumber,
        limits: HlsParserLimits,
    ) -> Result<Self, HlsParseError> {
        let split = split_attribute_list(raw)
            .ok_or_else(|| HlsParseError::new(HlsParseErrorKind::InvalidTagSyntax { line }))?;
        if split.len() > limits.max_attributes_per_tag() {
            return Err(HlsParseError::new(
                HlsParseErrorKind::AttributeLimitExceeded { line },
            ));
        }
        let mut names = HashSet::with_capacity(split.len());
        let mut entries = Vec::with_capacity(split.len());
        for attribute in split {
            let (name, value) = attribute
                .split_once('=')
                .ok_or_else(|| HlsParseError::new(HlsParseErrorKind::InvalidTagSyntax { line }))?;
            if has_unquoted_whitespace(attribute) {
                return Err(HlsParseError::new(
                    HlsParseErrorKind::WhitespaceNotAllowed { line },
                ));
            }
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
                || !valid_attribute_value(value)
            {
                return Err(HlsParseError::new(HlsParseErrorKind::InvalidTagSyntax {
                    line,
                }));
            }
            if !names.insert(name) {
                return Err(HlsParseError::new(HlsParseErrorKind::DuplicateAttribute {
                    line,
                }));
            }
            entries.push((name, value));
        }
        Ok(Self { entries })
    }

    pub(crate) fn raw(&self, name: &str) -> Option<&'a str> {
        self.entries
            .iter()
            .find_map(|(candidate, value)| (*candidate == name).then_some(*value))
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn quoted(&self, name: &str) -> Option<&'a str> {
        self.raw(name)
            .and_then(|value| value.strip_prefix('"'))
            .and_then(|value| value.strip_suffix('"'))
    }
}

fn split_attribute_list(value: &str) -> Option<Vec<&str>> {
    let mut attributes = Vec::new();
    let mut start = 0usize;
    let mut quoted = false;
    for (index, character) in value.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                attributes.push(value.get(start..index)?);
                start = index + 1;
            }
            '\r' | '\n' => return None,
            _ => {}
        }
    }
    if quoted {
        return None;
    }
    attributes.push(value.get(start..)?);
    (!attributes.iter().any(|attribute| attribute.is_empty())).then_some(attributes)
}

fn has_unquoted_whitespace(value: &str) -> bool {
    let mut quoted = false;
    for character in value.chars() {
        if character == '"' {
            quoted = !quoted;
        } else if !quoted && character.is_whitespace() {
            return true;
        }
    }
    false
}

fn valid_attribute_value(value: &str) -> bool {
    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        return !inner.contains('"') && !inner.contains('\r') && !inner.contains('\n');
    }
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'.' | b'_' | b'+' | b'/' | b':' | b'x')
        })
}
