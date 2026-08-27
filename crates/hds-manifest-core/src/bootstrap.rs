//! Bounded parser Adobe `abst`/`asrt`/`afrt` bootstrap timeline.

use std::num::NonZeroUsize;

use thiserror::Error;

/// Limits для binary bootstrap expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdsBootstrapLimits {
    /// Максимальный размер всей bootstrap bytes.
    pub maximum_bytes: NonZeroUsize,
    /// Максимальное число boxes.
    pub maximum_boxes: NonZeroUsize,
    /// Максимальное число expanded fragments.
    pub maximum_fragments: NonZeroUsize,
    /// Максимальная длина каждого null-terminated string.
    pub maximum_string_bytes: NonZeroUsize,
}

/// Один VOD fragment timeline row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdsFragment {
    /// HTTP URL segment number.
    segment: u32,
    /// HTTP URL fragment number.
    fragment: u32,
    /// Start timestamp в `timescale` units.
    timestamp: u64,
    /// Duration в `timescale` units.
    duration: u32,
}

impl HdsFragment {
    /// Создаёт fragment после binary bounds checks.
    #[must_use]
    pub const fn new(segment: u32, fragment: u32, timestamp: u64, duration: u32) -> Self {
        Self {
            segment,
            fragment,
            timestamp,
            duration,
        }
    }

    /// Возвращает segment number.
    #[must_use]
    pub const fn segment(self) -> u32 {
        self.segment
    }

    /// Возвращает fragment number.
    #[must_use]
    pub const fn fragment(self) -> u32 {
        self.fragment
    }

    /// Возвращает timestamp.
    #[must_use]
    pub const fn timestamp(self) -> u64 {
        self.timestamp
    }

    /// Возвращает duration.
    #[must_use]
    pub const fn duration(self) -> u32 {
        self.duration
    }
}

/// Один compact segment-run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdsSegmentRun {
    /// First segment number covered by this run.
    pub first_segment: u32,
    /// Fragments in every segment of this run.
    pub fragments_per_segment: u32,
}

/// Один compact fragment-run до expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdsFragmentRun {
    /// First fragment number covered by this run.
    pub first_fragment: u32,
    /// First fragment timestamp.
    pub first_timestamp: u64,
    /// Common duration; zero means end marker and is not a media fragment.
    pub duration: u32,
    /// Discontinuity kind присутствует только у zero-duration row.
    pub discontinuity: Option<u8>,
}

/// Parsed VOD timeline for one quality modifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdsBootstrapTimeline {
    /// Bootstrap live flag; S38 runtime requires false.
    live: bool,
    /// Timestamp units per second.
    timescale: u32,
    /// Expanded ordered VOD fragments.
    fragments: Box<[HdsFragment]>,
}

impl HdsBootstrapTimeline {
    /// Возвращает live flag.
    #[must_use]
    pub const fn live(&self) -> bool {
        self.live
    }

    /// Возвращает timescale.
    #[must_use]
    pub const fn timescale(&self) -> u32 {
        self.timescale
    }

    /// Возвращает expanded fragments.
    #[must_use]
    pub fn fragments(&self) -> &[HdsFragment] {
        &self.fragments
    }

    /// Создаёт timeline для tests/domain adapters.
    #[must_use]
    pub fn from_parts(live: bool, timescale: u32, fragments: Vec<HdsFragment>) -> Self {
        Self {
            live,
            timescale,
            fragments: fragments.into_boxed_slice(),
        }
    }
}

/// Binary bootstrap failure с bounded category.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HdsBootstrapError {
    /// Input/box structure truncated or inconsistent.
    #[error("HDS bootstrap binary structure is malformed")]
    Malformed,
    /// Input exceeded caller-owned bound.
    #[error("HDS bootstrap exceeds the configured limit")]
    LimitExceeded,
    /// Unsupported box/profile semantics.
    #[error("HDS bootstrap contains an unsupported profile or table")]
    Unsupported,
}

/// Парсит named-access bootstrap и разворачивает одну quality timeline.
pub fn parse_bootstrap(
    input: &[u8],
    quality_modifier: &str,
    limits: HdsBootstrapLimits,
) -> Result<HdsBootstrapTimeline, HdsBootstrapError> {
    if input.len() > limits.maximum_bytes.get() {
        return Err(HdsBootstrapError::LimitExceeded);
    }
    let boxes = collect_boxes(input, limits)?;
    let abst = boxes
        .iter()
        .find(|item| item.kind == *b"abst")
        .ok_or(HdsBootstrapError::Malformed)?;
    parse_abst(abst.payload, quality_modifier, limits)
}

/// Lightweight box view без копирования payload.
#[derive(Clone, Copy)]
struct BoxView<'a> {
    kind: [u8; 4],
    payload: &'a [u8],
}

/// Снимает top-level boxes bounded by count and size.
fn collect_boxes<'a>(
    input: &'a [u8],
    limits: HdsBootstrapLimits,
) -> Result<Vec<BoxView<'a>>, HdsBootstrapError> {
    let mut cursor = 0usize;
    let mut boxes = Vec::new();
    while cursor < input.len() {
        if boxes.len() >= limits.maximum_boxes.get() {
            return Err(HdsBootstrapError::LimitExceeded);
        }
        let (next, item) = read_box(input, cursor)?;
        boxes.push(item);
        cursor = next;
    }
    Ok(boxes)
}

/// Читает ISO box header с 32/64-bit size support.
fn read_box<'a>(input: &'a [u8], offset: usize) -> Result<(usize, BoxView<'a>), HdsBootstrapError> {
    let header_end = offset.checked_add(8).ok_or(HdsBootstrapError::Malformed)?;
    let header = input
        .get(offset..header_end)
        .ok_or(HdsBootstrapError::Malformed)?;
    let size32 = u32::from_be_bytes(header[0..4].try_into().expect("8-byte box header"));
    let kind = header[4..8].try_into().expect("4-byte box kind");
    let (header_bytes, box_size) = match size32 {
        0 => (8usize, input.len().saturating_sub(offset)),
        1 => {
            let large_end = offset.checked_add(16).ok_or(HdsBootstrapError::Malformed)?;
            let large = input
                .get(offset + 8..large_end)
                .ok_or(HdsBootstrapError::Malformed)?;
            let large = u64::from_be_bytes(large.try_into().expect("8-byte large size"));
            (
                16usize,
                usize::try_from(large).map_err(|_| HdsBootstrapError::LimitExceeded)?,
            )
        }
        value => (8usize, usize::try_from(value).expect("u32 fits usize")),
    };
    if box_size < header_bytes {
        return Err(HdsBootstrapError::Malformed);
    }
    let end = offset
        .checked_add(box_size)
        .ok_or(HdsBootstrapError::Malformed)?;
    let payload_start = offset
        .checked_add(header_bytes)
        .ok_or(HdsBootstrapError::Malformed)?;
    let payload = input
        .get(payload_start..end)
        .ok_or(HdsBootstrapError::Malformed)?;
    Ok((end, BoxView { kind, payload }))
}

/// Читает abst fixed header и embedded asrt/afrt boxes.
fn parse_abst(
    payload: &[u8],
    quality_modifier: &str,
    limits: HdsBootstrapLimits,
) -> Result<HdsBootstrapTimeline, HdsBootstrapError> {
    let mut reader = Cursor::new(payload);
    validate_full_box_header(reader.take_u32()?)?;
    let _bootstrap_version = reader.take_u32()?;
    let profile_live_update = reader.take_u8()?;
    let live = profile_live_update & 0x20 != 0;
    let profile = profile_live_update & 0xC0;
    let update_or_reserved = profile_live_update & 0x1F;
    if profile != 0 || update_or_reserved != 0 {
        return Err(HdsBootstrapError::Unsupported);
    }
    let timescale = reader.take_u32()?;
    if timescale == 0 {
        return Err(HdsBootstrapError::Malformed);
    }
    let _current_media_time = reader.take_u64()?;
    let _smpte_offset = reader.take_u64()?;
    let _movie_identifier = reader.take_string(limits)?;
    let server_count = reader.take_u8()? as usize;
    for _ in 0..server_count {
        let _ = reader.take_string(limits)?;
    }
    let quality_count = reader.take_u8()? as usize;
    let mut quality_names = Vec::with_capacity(quality_count);
    for _ in 0..quality_count {
        quality_names.push(reader.take_string(limits)?);
    }
    let _drm = reader.take_string(limits)?;
    let _metadata = reader.take_string(limits)?;
    let segment_table_count = reader.take_u8()? as usize;
    if segment_table_count == 0 {
        return Err(HdsBootstrapError::Malformed);
    }
    if segment_table_count > limits.maximum_boxes.get() {
        return Err(HdsBootstrapError::LimitExceeded);
    }
    let mut segment_tables = Vec::with_capacity(segment_table_count);
    for _ in 0..segment_table_count {
        let box_bytes = reader.take_embedded_box()?;
        segment_tables.push(parse_asrt(box_bytes, &quality_names, limits)?);
    }
    let fragment_table_count = reader.take_u8()? as usize;
    if fragment_table_count == 0 {
        return Err(HdsBootstrapError::Malformed);
    }
    if fragment_table_count > limits.maximum_boxes.get() {
        return Err(HdsBootstrapError::LimitExceeded);
    }
    let mut fragment_tables = Vec::with_capacity(fragment_table_count);
    for _ in 0..fragment_table_count {
        let box_bytes = reader.take_embedded_box()?;
        fragment_tables.push(parse_afrt(box_bytes, &quality_names, limits)?);
    }
    if !reader.is_empty() {
        return Err(HdsBootstrapError::Malformed);
    }
    let segment_runs = select_segment_runs(segment_tables, quality_modifier)?;
    let fragment_table = select_fragment_table(fragment_tables, quality_modifier)?;
    if fragment_table.timescale != timescale {
        return Err(HdsBootstrapError::Malformed);
    }
    let fragments = expand_fragments(
        &segment_runs,
        &fragment_table,
        limits.maximum_fragments.get(),
    )?;
    Ok(HdsBootstrapTimeline::from_parts(live, timescale, fragments))
}

/// Internal asrt table with optional quality scoping.
struct ParsedSegmentTable {
    qualities: Vec<String>,
    runs: Vec<HdsSegmentRun>,
}

/// Internal afrt table with optional quality scoping.
struct ParsedFragmentTable {
    qualities: Vec<String>,
    timescale: u32,
    runs: Vec<HdsFragmentRun>,
}

/// Parses one embedded asrt box.
fn parse_asrt(
    bytes: &[u8],
    quality_names: &[String],
    limits: HdsBootstrapLimits,
) -> Result<ParsedSegmentTable, HdsBootstrapError> {
    let boxes = collect_boxes(bytes, limits)?;
    let [item] = boxes.as_slice() else {
        return Err(HdsBootstrapError::Malformed);
    };
    if item.kind != *b"asrt" {
        return Err(HdsBootstrapError::Malformed);
    }
    let mut reader = Cursor::new(item.payload);
    validate_full_box_header(reader.take_u32()?)?;
    let qualities = take_quality_names(&mut reader, quality_names, limits)?;
    let count = bounded_table_count(reader.take_u32()?, limits)?;
    let mut runs = Vec::with_capacity(count);
    for _ in 0..count {
        let first_segment = reader.take_u32()?;
        let fragments_per_segment = reader.take_u32()?;
        if fragments_per_segment == 0
            || runs
                .last()
                .is_some_and(|run: &HdsSegmentRun| run.first_segment >= first_segment)
        {
            return Err(HdsBootstrapError::Malformed);
        }
        runs.push(HdsSegmentRun {
            first_segment,
            fragments_per_segment,
        });
    }
    if !reader.is_empty() {
        return Err(HdsBootstrapError::Malformed);
    }
    Ok(ParsedSegmentTable { qualities, runs })
}

/// Parses one embedded afrt box.
fn parse_afrt(
    bytes: &[u8],
    quality_names: &[String],
    limits: HdsBootstrapLimits,
) -> Result<ParsedFragmentTable, HdsBootstrapError> {
    let boxes = collect_boxes(bytes, limits)?;
    let [item] = boxes.as_slice() else {
        return Err(HdsBootstrapError::Malformed);
    };
    if item.kind != *b"afrt" {
        return Err(HdsBootstrapError::Malformed);
    }
    let mut reader = Cursor::new(item.payload);
    validate_full_box_header(reader.take_u32()?)?;
    let timescale = reader.take_u32()?;
    if timescale == 0 {
        return Err(HdsBootstrapError::Malformed);
    }
    let qualities = take_quality_names(&mut reader, quality_names, limits)?;
    let count = bounded_table_count(reader.take_u32()?, limits)?;
    let mut runs = Vec::with_capacity(count);
    for _ in 0..count {
        let first_fragment = reader.take_u32()?;
        let first_timestamp = reader.take_u64()?;
        let duration = reader.take_u32()?;
        let discontinuity = (duration == 0).then(|| reader.take_u8()).transpose()?;
        if discontinuity.is_some_and(|indicator| indicator > 3) {
            return Err(HdsBootstrapError::Unsupported);
        }
        // Порядок media-runs проверяется после чтения всей таблицы. Zero-duration
        // `END_OF_PRESENTATION` — управляющая запись, и её идентификатор по формату
        // не обязан продолжать последовательность реальных media fragment-ов.
        runs.push(HdsFragmentRun {
            first_fragment,
            first_timestamp,
            duration,
            discontinuity,
        });
    }
    if !reader.is_empty() {
        return Err(HdsBootstrapError::Malformed);
    }
    Ok(ParsedFragmentTable {
        qualities,
        timescale,
        runs,
    })
}

/// Проверяет supported full-box version и запрещает update flags.
fn validate_full_box_header(version_flags: u32) -> Result<(), HdsBootstrapError> {
    let version = version_flags >> 24;
    let flags = version_flags & 0x00FF_FFFF;
    if version > 1 || flags != 0 {
        return Err(HdsBootstrapError::Unsupported);
    }
    Ok(())
}

/// Проверяет untrusted u32 count до выделения памяти.
fn bounded_table_count(
    serialized_count: u32,
    limits: HdsBootstrapLimits,
) -> Result<usize, HdsBootstrapError> {
    let count = usize::try_from(serialized_count).map_err(|_| HdsBootstrapError::LimitExceeded)?;
    if count == 0 {
        return Err(HdsBootstrapError::Malformed);
    }
    if count > limits.maximum_fragments.get() {
        return Err(HdsBootstrapError::LimitExceeded);
    }
    Ok(count)
}

/// Разбирает quality references и проверяет их against abst table.
fn take_quality_names(
    reader: &mut Cursor<'_>,
    all_names: &[String],
    limits: HdsBootstrapLimits,
) -> Result<Vec<String>, HdsBootstrapError> {
    let count = reader.take_u8()? as usize;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let name = reader.take_string(limits)?;
        if !all_names.is_empty() && !all_names.iter().any(|candidate| candidate == &name) {
            return Err(HdsBootstrapError::Malformed);
        }
        names.push(name);
    }
    Ok(names)
}

/// Selects the only applicable table or exact quality-scoped table.
fn select_segment_runs(
    tables: Vec<ParsedSegmentTable>,
    quality: &str,
) -> Result<Vec<HdsSegmentRun>, HdsBootstrapError> {
    let applicable = tables
        .into_iter()
        .filter(|table| {
            table.qualities.is_empty() || table.qualities.iter().any(|name| name == quality)
        })
        .collect::<Vec<_>>();
    if applicable.len() != 1 {
        return Err(HdsBootstrapError::Unsupported);
    }
    Ok(applicable
        .into_iter()
        .next()
        .expect("one applicable asrt")
        .runs)
}

/// Selects the only applicable fragment table or exact quality-scoped table.
fn select_fragment_table(
    tables: Vec<ParsedFragmentTable>,
    quality: &str,
) -> Result<ParsedFragmentTable, HdsBootstrapError> {
    let mut applicable = tables
        .into_iter()
        .filter(|table| {
            table.qualities.is_empty() || table.qualities.iter().any(|name| name == quality)
        })
        .collect::<Vec<_>>();
    if applicable.len() != 1 {
        return Err(HdsBootstrapError::Unsupported);
    }
    Ok(applicable.pop().expect("one applicable afrt"))
}

/// Разворачивает compact afrt runs и назначает segment number через asrt.
fn expand_fragments(
    segment_runs: &[HdsSegmentRun],
    fragment_table: &ParsedFragmentTable,
    maximum_fragments: usize,
) -> Result<Vec<HdsFragment>, HdsBootstrapError> {
    if segment_runs.is_empty() || fragment_table.runs.is_empty() {
        return Err(HdsBootstrapError::Malformed);
    }
    let first_fragment = fragment_table.runs[0].first_fragment;
    if fragment_table.runs[0].duration == 0 {
        return Err(HdsBootstrapError::Malformed);
    }
    let last_fragment = last_advertised_fragment(first_fragment, segment_runs)?;
    let end_exclusive = last_fragment
        .checked_add(1)
        .ok_or(HdsBootstrapError::Malformed)?;
    validate_fragment_runs(&fragment_table.runs, first_fragment, end_exclusive)?;
    let mut fragments = Vec::new();
    for (index, run) in fragment_table.runs.iter().enumerate() {
        if run.duration == 0 {
            continue;
        }
        let run_end = fragment_table
            .runs
            .get(index + 1..)
            .and_then(|remaining_runs| {
                remaining_runs
                    .iter()
                    .find(|next_run| next_run.duration != 0)
            })
            .map_or(end_exclusive, |next| next.first_fragment);
        if run_end <= run.first_fragment || run_end > end_exclusive {
            return Err(HdsBootstrapError::Malformed);
        }
        let mut timestamp = run.first_timestamp;
        for fragment_number in run.first_fragment..run_end {
            if fragments.len() >= maximum_fragments {
                return Err(HdsBootstrapError::LimitExceeded);
            }
            let segment = segment_for_fragment(fragment_number, first_fragment, segment_runs)?;
            fragments.push(HdsFragment::new(
                segment,
                fragment_number,
                timestamp,
                run.duration,
            ));
            timestamp = timestamp
                .checked_add(u64::from(run.duration))
                .ok_or(HdsBootstrapError::Malformed)?;
        }
    }
    if fragments.is_empty() {
        return Err(HdsBootstrapError::Malformed);
    }
    if fragments.last().map(|fragment| fragment.fragment()) != Some(last_fragment) {
        return Err(HdsBootstrapError::Malformed);
    }
    Ok(fragments)
}

/// Проверяет strict VOD subset: только optional terminal end marker без gaps.
fn validate_fragment_runs(
    runs: &[HdsFragmentRun],
    first_fragment: u32,
    end_exclusive: u32,
) -> Result<(), HdsBootstrapError> {
    for (index, run) in runs.iter().enumerate() {
        if run.duration == 0 {
            let marker_is_outside_media_range =
                run.first_fragment < first_fragment || run.first_fragment >= end_exclusive;
            let is_terminal_end = index + 1 == runs.len()
                && run.discontinuity == Some(0)
                && marker_is_outside_media_range;
            if !is_terminal_end {
                return Err(HdsBootstrapError::Unsupported);
            }
            continue;
        }
        let next_media_run = runs.get(index + 1..).and_then(|remaining_runs| {
            remaining_runs
                .iter()
                .find(|next_run| next_run.duration != 0)
        });
        if run.discontinuity.is_some()
            || run.first_fragment < first_fragment
            || run.first_fragment >= end_exclusive
            || next_media_run.is_some_and(|next_run| {
                next_run.first_fragment <= run.first_fragment
                    || next_run.first_timestamp <= run.first_timestamp
            })
        {
            return Err(HdsBootstrapError::Malformed);
        }
    }
    Ok(())
}

/// Вычисляет последний advertised fragment из compact segment table.
fn last_advertised_fragment(
    first_fragment: u32,
    runs: &[HdsSegmentRun],
) -> Result<u32, HdsBootstrapError> {
    let mut fragment_cursor = u64::from(first_fragment);
    for (index, run) in runs.iter().enumerate() {
        let segment_count = runs.get(index + 1).map_or(1_u64, |next| {
            u64::from(next.first_segment - run.first_segment)
        });
        let fragment_count = segment_count
            .checked_mul(u64::from(run.fragments_per_segment))
            .ok_or(HdsBootstrapError::Malformed)?;
        let end_exclusive = fragment_cursor
            .checked_add(fragment_count)
            .ok_or(HdsBootstrapError::Malformed)?;
        if index + 1 == runs.len() {
            let last = end_exclusive
                .checked_sub(1)
                .ok_or(HdsBootstrapError::Malformed)?;
            return u32::try_from(last).map_err(|_| HdsBootstrapError::Malformed);
        }
        fragment_cursor = end_exclusive;
    }
    Err(HdsBootstrapError::Malformed)
}

/// Находит segment ID, используя compact asrt runs.
fn segment_for_fragment(
    fragment: u32,
    first_fragment: u32,
    runs: &[HdsSegmentRun],
) -> Result<u32, HdsBootstrapError> {
    let mut fragment_cursor = u64::from(first_fragment);
    for (index, run) in runs.iter().enumerate() {
        let next_segment = runs.get(index + 1).map(|next| next.first_segment);
        let segment_count = next_segment.map_or(1u64, |next| {
            u64::from(next.saturating_sub(run.first_segment))
        });
        let range_fragments = segment_count
            .checked_mul(u64::from(run.fragments_per_segment))
            .ok_or(HdsBootstrapError::Malformed)?;
        let end = fragment_cursor
            .checked_add(range_fragments)
            .ok_or(HdsBootstrapError::Malformed)?;
        let fragment_value = u64::from(fragment);
        if fragment_value >= fragment_cursor && fragment_value < end {
            let segment_offset =
                (fragment_value - fragment_cursor) / u64::from(run.fragments_per_segment);
            let segment = u64::from(run.first_segment)
                .checked_add(segment_offset)
                .ok_or(HdsBootstrapError::Malformed)?;
            return u32::try_from(segment).map_err(|_| HdsBootstrapError::Malformed);
        }
        fragment_cursor = end;
    }
    Err(HdsBootstrapError::Malformed)
}

/// Tiny bounded big-endian cursor.
struct Cursor<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    /// Creates a cursor over one box payload.
    const fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    /// Reads u8.
    fn take_u8(&mut self) -> Result<u8, HdsBootstrapError> {
        let value = *self
            .input
            .get(self.offset)
            .ok_or(HdsBootstrapError::Malformed)?;
        self.offset += 1;
        Ok(value)
    }

    /// Reads u32.
    fn take_u32(&mut self) -> Result<u32, HdsBootstrapError> {
        let bytes = self.take_bytes(4)?;
        Ok(u32::from_be_bytes(bytes.try_into().expect("four bytes")))
    }

    /// Reads u64.
    fn take_u64(&mut self) -> Result<u64, HdsBootstrapError> {
        let bytes = self.take_bytes(8)?;
        Ok(u64::from_be_bytes(bytes.try_into().expect("eight bytes")))
    }

    /// Reads a null-terminated UTF-8 string.
    fn take_string(&mut self, limits: HdsBootstrapLimits) -> Result<String, HdsBootstrapError> {
        let remainder = self
            .input
            .get(self.offset..)
            .ok_or(HdsBootstrapError::Malformed)?;
        let relative_end = remainder
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(HdsBootstrapError::Malformed)?;
        if relative_end > limits.maximum_string_bytes.get() {
            return Err(HdsBootstrapError::LimitExceeded);
        }
        let end = self.offset + relative_end;
        let value = std::str::from_utf8(&self.input[self.offset..end])
            .map_err(|_| HdsBootstrapError::Malformed)?
            .to_owned();
        self.offset = end + 1;
        Ok(value)
    }

    /// Reads one complete embedded box.
    fn take_embedded_box(&mut self) -> Result<&'a [u8], HdsBootstrapError> {
        let start = self.offset;
        let (end, _) = read_box(self.input, start)?;
        self.offset = end;
        Ok(&self.input[start..end])
    }

    /// Checks exact payload exhaustion.
    const fn is_empty(&self) -> bool {
        self.offset == self.input.len()
    }

    /// Reads an exact byte slice.
    fn take_bytes(&mut self, length: usize) -> Result<&'a [u8], HdsBootstrapError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(HdsBootstrapError::Malformed)?;
        let bytes = self
            .input
            .get(self.offset..end)
            .ok_or(HdsBootstrapError::Malformed)?;
        self.offset = end;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests;
