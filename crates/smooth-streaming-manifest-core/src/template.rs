//! Typed relative fragment URL template grammar.

use std::fmt;

use crate::custom_attributes::SmoothCustomAttributeSet;
use crate::error::{SmoothManifestError, SmoothUrlTemplateError};
use crate::limits::SmoothManifestLimits;

/// CustomAttributes rendering никогда не принимает arbitrary replacement string.
#[derive(Debug, Clone, Copy)]
pub enum SmoothCustomAttributesRender<'attributes> {
    Unavailable,
    Values(&'attributes SmoothCustomAttributeSet),
}

/// Именованный render context для одного fragment path.
#[derive(Debug, Clone, Copy)]
pub struct SmoothFragmentUrlRenderContext<'attributes> {
    bitrate: u64,
    start_time_ticks: u64,
    custom_attributes: SmoothCustomAttributesRender<'attributes>,
}

impl<'attributes> SmoothFragmentUrlRenderContext<'attributes> {
    #[must_use]
    pub const fn new(
        bitrate: u64,
        start_time_ticks: u64,
        custom_attributes: SmoothCustomAttributesRender<'attributes>,
    ) -> Self {
        Self {
            bitrate,
            start_time_ticks,
            custom_attributes,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum SmoothFragmentUrlPart {
    Literal(Box<str>),
    Bitrate,
    StartTime,
    CustomAttributes,
}

/// Compiled template хранит только typed parts и не экспортирует raw replace API.
#[derive(Clone, PartialEq, Eq)]
pub struct SmoothFragmentUrlTemplate {
    parts: Box<[SmoothFragmentUrlPart]>,
    maximum_rendered_bytes: usize,
}

impl SmoothFragmentUrlTemplate {
    pub(crate) fn parse(
        template: &str,
        limits: &SmoothManifestLimits,
    ) -> Result<Self, SmoothManifestError> {
        validate_relative_template(template, limits.maximum_template_bytes())?;
        let mut parts = Vec::new();
        let mut cursor = 0usize;
        let mut bitrate_seen = false;
        let mut start_time_seen = false;
        let mut custom_attributes_seen = false;

        while let Some(relative_open) = template[cursor..].find('{') {
            let open = cursor + relative_open;
            if open > cursor {
                if template[cursor..open].contains('}') {
                    return Err(invalid_template(
                        SmoothUrlTemplateError::UnterminatedPlaceholder,
                    ));
                }
                parts.push(SmoothFragmentUrlPart::Literal(
                    template[cursor..open].to_owned().into_boxed_str(),
                ));
            }
            let close = template[open + 1..]
                .find('}')
                .map(|relative_close| open + 1 + relative_close)
                .ok_or_else(|| invalid_template(SmoothUrlTemplateError::UnterminatedPlaceholder))?;
            let placeholder = &template[open + 1..close];
            let part = match placeholder {
                "bitrate" | "Bitrate" => {
                    reject_duplicate(&mut bitrate_seen)?;
                    SmoothFragmentUrlPart::Bitrate
                }
                "start time" | "start_time" => {
                    reject_duplicate(&mut start_time_seen)?;
                    SmoothFragmentUrlPart::StartTime
                }
                "CustomAttributes" => {
                    reject_duplicate(&mut custom_attributes_seen)?;
                    SmoothFragmentUrlPart::CustomAttributes
                }
                _ => return Err(invalid_template(SmoothUrlTemplateError::UnknownPlaceholder)),
            };
            parts.push(part);
            cursor = close + 1;
        }
        if template[cursor..].contains('}') {
            return Err(invalid_template(
                SmoothUrlTemplateError::UnterminatedPlaceholder,
            ));
        }
        if cursor < template.len() {
            parts.push(SmoothFragmentUrlPart::Literal(
                template[cursor..].to_owned().into_boxed_str(),
            ));
        }
        if !bitrate_seen {
            return Err(invalid_template(
                SmoothUrlTemplateError::MissingBitratePlaceholder,
            ));
        }
        if !start_time_seen {
            return Err(invalid_template(
                SmoothUrlTemplateError::MissingStartTimePlaceholder,
            ));
        }
        Ok(Self {
            parts: parts.into_boxed_slice(),
            maximum_rendered_bytes: limits.maximum_template_bytes(),
        })
    }

    pub fn render_fragment_path(
        &self,
        context: SmoothFragmentUrlRenderContext<'_>,
    ) -> Result<String, SmoothManifestError> {
        let mut output = String::new();
        for part in &self.parts {
            match part {
                SmoothFragmentUrlPart::Literal(literal) => output.push_str(literal),
                SmoothFragmentUrlPart::Bitrate => {
                    append_u64(&mut output, context.bitrate);
                }
                SmoothFragmentUrlPart::StartTime => {
                    append_u64(&mut output, context.start_time_ticks);
                }
                SmoothFragmentUrlPart::CustomAttributes => match context.custom_attributes {
                    SmoothCustomAttributesRender::Unavailable => {
                        return Err(invalid_template(
                            SmoothUrlTemplateError::CustomAttributesUnavailable,
                        ));
                    }
                    SmoothCustomAttributesRender::Values(attributes) => {
                        attributes.append_template_component(&mut output);
                    }
                },
            }
            if output.len() > self.maximum_rendered_bytes {
                return Err(invalid_template(
                    SmoothUrlTemplateError::RenderedPathTooLong,
                ));
            }
        }
        Ok(output)
    }

    /// Model admission использует только факт участия attributes в render identity.
    #[must_use]
    pub(crate) fn uses_custom_attributes(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, SmoothFragmentUrlPart::CustomAttributes))
    }
}

impl fmt::Debug for SmoothFragmentUrlTemplate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothFragmentUrlTemplate")
            .field("part_count", &self.parts.len())
            .finish()
    }
}

fn validate_relative_template(
    template: &str,
    maximum_bytes: usize,
) -> Result<(), SmoothManifestError> {
    if template.is_empty() {
        return Err(invalid_template(SmoothUrlTemplateError::Empty));
    }
    if template.len() > maximum_bytes {
        return Err(invalid_template(SmoothUrlTemplateError::TooLong));
    }
    if template.chars().any(char::is_control) {
        return Err(invalid_template(SmoothUrlTemplateError::ControlCharacter));
    }
    if template.starts_with('/') || template.contains("://") || template.contains(':') {
        return Err(invalid_template(SmoothUrlTemplateError::AbsoluteReference));
    }
    if template.contains('?') || template.contains('#') {
        return Err(invalid_template(SmoothUrlTemplateError::QueryOrFragment));
    }
    if template.contains('\\') {
        return Err(invalid_template(SmoothUrlTemplateError::Backslash));
    }
    let lowercase = template.to_ascii_lowercase();
    if lowercase.contains("%2e") || lowercase.contains("%2f") || lowercase.contains("%5c") {
        return Err(invalid_template(SmoothUrlTemplateError::Traversal));
    }
    if template
        .split('/')
        .any(|segment| matches!(segment, "." | ".."))
    {
        return Err(invalid_template(SmoothUrlTemplateError::Traversal));
    }
    Ok(())
}

fn reject_duplicate(seen: &mut bool) -> Result<(), SmoothManifestError> {
    if *seen {
        return Err(invalid_template(
            SmoothUrlTemplateError::DuplicatePlaceholder,
        ));
    }
    *seen = true;
    Ok(())
}

fn append_u64(output: &mut String, value: u64) {
    use std::fmt::Write as _;
    write!(output, "{value}").expect("String formatting infallible");
}

fn invalid_template(reason: SmoothUrlTemplateError) -> SmoothManifestError {
    SmoothManifestError::InvalidUrlTemplate { reason }
}
