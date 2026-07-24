use crate::model::DashTimelineEntry;

/// Максимальная ширина numeric format specifier.
const MAXIMUM_FORMAT_WIDTH: usize = 20;

/// Проверенная DASH template строка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashTemplateString(String);

/// Значения одного template expansion.
#[derive(Debug, Clone, Copy)]
pub struct DashTemplateContext<'a> {
    /// Representation identifier.
    pub representation_id: &'a str,
    /// Representation bandwidth.
    pub bandwidth: Option<u64>,
    /// Segment number.
    pub number: u64,
    /// Segment start time.
    pub time: u64,
}

/// Один expanded timeline segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashSegmentPoint {
    /// `$Number$` value.
    pub number: u64,
    /// `$Time$` value.
    pub start_time: u64,
    /// Duration в template timescale.
    pub duration: u64,
}

/// Result bounded timeline expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashTimelineExpansion {
    /// Ordered points.
    pub segments: Box<[DashSegmentPoint]>,
}

/// Checked template/timeline error без исходной строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DashTemplateError {
    /// Placeholder syntax неизвестен или malformed.
    #[error("invalid DASH template")]
    InvalidSyntax,
    /// Требуемого context field нет.
    #[error("DASH template value is unavailable")]
    MissingValue,
    /// Expansion превысил caller cap.
    #[error("DASH timeline expansion limit exceeded")]
    ExpansionLimit,
    /// Integer arithmetic overflow.
    #[error("DASH timeline arithmetic overflow")]
    ArithmeticOverflow,
    /// `r=-1` не имеет следующей границы или Period end.
    #[error("unbounded DASH timeline repeat")]
    UnboundedRepeat,
}

impl DashTemplateString {
    /// Валидирует все placeholders один раз на parser boundary.
    pub fn parse(value: String) -> Result<Self, DashTemplateError> {
        validate_template(&value)?;
        Ok(Self(value))
    }

    /// Возвращает lexical template для diagnostics-free inspection.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Материализует один resource reference с checked formatting.
    pub fn expand(&self, context: DashTemplateContext<'_>) -> Result<String, DashTemplateError> {
        expand_template(&self.0, context)
    }
}

/// Раскрывает timeline строго внутри caller cap и optional Period boundary.
pub fn expand_timeline(
    entries: &[DashTimelineEntry],
    start_number: u64,
    period_end_time: Option<u64>,
    maximum_segments: usize,
) -> Result<DashTimelineExpansion, DashTemplateError> {
    let mut segments = Vec::new();
    let mut continuation_time = 0_u64;
    for (entry_index, entry) in entries.iter().enumerate() {
        let entry_start = entry.start_time.unwrap_or(continuation_time);
        let repeat_count = if entry.repeat >= 0 {
            u64::try_from(entry.repeat).map_err(|_| DashTemplateError::ArithmeticOverflow)?
        } else if entry.repeat == -1 {
            let boundary = entries
                .get(entry_index + 1)
                .and_then(|next| next.start_time)
                .or(period_end_time)
                .ok_or(DashTemplateError::UnboundedRepeat)?;
            if boundary <= entry_start {
                return Err(DashTemplateError::UnboundedRepeat);
            }
            let span = boundary
                .checked_sub(entry_start)
                .ok_or(DashTemplateError::ArithmeticOverflow)?;
            if span % entry.duration != 0 {
                return Err(DashTemplateError::UnboundedRepeat);
            }
            span.checked_div(entry.duration)
                .and_then(|count| count.checked_sub(1))
                .ok_or(DashTemplateError::ArithmeticOverflow)?
        } else {
            return Err(DashTemplateError::InvalidSyntax);
        };
        let segment_count = repeat_count
            .checked_add(1)
            .ok_or(DashTemplateError::ArithmeticOverflow)?;
        let segment_count_usize =
            usize::try_from(segment_count).map_err(|_| DashTemplateError::ExpansionLimit)?;
        if segments.len().saturating_add(segment_count_usize) > maximum_segments {
            return Err(DashTemplateError::ExpansionLimit);
        }
        for repeat_index in 0..segment_count {
            let delta = entry
                .duration
                .checked_mul(repeat_index)
                .ok_or(DashTemplateError::ArithmeticOverflow)?;
            let start_time = entry_start
                .checked_add(delta)
                .ok_or(DashTemplateError::ArithmeticOverflow)?;
            let ordinal =
                u64::try_from(segments.len()).map_err(|_| DashTemplateError::ArithmeticOverflow)?;
            let number = start_number
                .checked_add(ordinal)
                .ok_or(DashTemplateError::ArithmeticOverflow)?;
            segments.push(DashSegmentPoint {
                number,
                start_time,
                duration: entry.duration,
            });
        }
        continuation_time = entry_start
            .checked_add(
                entry
                    .duration
                    .checked_mul(segment_count)
                    .ok_or(DashTemplateError::ArithmeticOverflow)?,
            )
            .ok_or(DashTemplateError::ArithmeticOverflow)?;
    }
    Ok(DashTimelineExpansion {
        segments: segments.into_boxed_slice(),
    })
}

/// Проверяет placeholders без allocation результата.
fn validate_template(value: &str) -> Result<(), DashTemplateError> {
    expand_template(
        value,
        DashTemplateContext {
            representation_id: "representation",
            bandwidth: Some(1),
            number: 1,
            time: 1,
        },
    )
    .map(|_| ())
}

/// Общий checked scanner для validation и expansion.
fn expand_template(
    value: &str,
    context: DashTemplateContext<'_>,
) -> Result<String, DashTemplateError> {
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(open_index) = remaining.find('$') {
        output.push_str(&remaining[..open_index]);
        remaining = &remaining[open_index + 1..];
        if let Some(after_escape) = remaining.strip_prefix('$') {
            output.push('$');
            remaining = after_escape;
            continue;
        }
        let close_index = remaining
            .find('$')
            .ok_or(DashTemplateError::InvalidSyntax)?;
        let placeholder = &remaining[..close_index];
        remaining = &remaining[close_index + 1..];
        expand_placeholder(placeholder, context, &mut output)?;
    }
    output.push_str(remaining);
    Ok(output)
}

/// Раскрывает один identifier и optional `%0Nd`.
fn expand_placeholder(
    placeholder: &str,
    context: DashTemplateContext<'_>,
    output: &mut String,
) -> Result<(), DashTemplateError> {
    let (identifier, width) = parse_placeholder_format(placeholder)?;
    match identifier {
        "RepresentationID" if width.is_none() => output.push_str(context.representation_id),
        "Bandwidth" => push_number(
            output,
            context.bandwidth.ok_or(DashTemplateError::MissingValue)?,
            width,
        ),
        "Number" => push_number(output, context.number, width),
        "Time" => push_number(output, context.time, width),
        _ => return Err(DashTemplateError::InvalidSyntax),
    }
    Ok(())
}

/// Разбирает restricted printf width, разрешённый DASH identifier-ам.
fn parse_placeholder_format(placeholder: &str) -> Result<(&str, Option<usize>), DashTemplateError> {
    let Some(percent_index) = placeholder.find('%') else {
        return Ok((placeholder, None));
    };
    let identifier = &placeholder[..percent_index];
    let specifier = &placeholder[percent_index..];
    if !specifier.starts_with("%0") || !specifier.ends_with('d') {
        return Err(DashTemplateError::InvalidSyntax);
    }
    let width_text = &specifier[2..specifier.len() - 1];
    let width = width_text
        .parse::<usize>()
        .map_err(|_| DashTemplateError::InvalidSyntax)?;
    if width == 0 || width > MAXIMUM_FORMAT_WIDTH {
        return Err(DashTemplateError::InvalidSyntax);
    }
    Ok((identifier, Some(width)))
}

/// Форматирует число без locale/magic formatting.
fn push_number(output: &mut String, value: u64, width: Option<usize>) {
    match width {
        Some(width) => output.push_str(&format!("{value:0width$}")),
        None => output.push_str(&value.to_string()),
    }
}
