use std::collections::HashSet;

use url::Url;

use crate::{
    ByteRange, ExactReference, HlsDuration, HlsKeyDeclaration, HlsKeyMethod, HlsLineNumber,
    HlsParseError, HlsParseErrorKind, HlsParserLimits, HlsPlaylist, HlsPlaylistType,
    InitializationMap, MasterPlaylist, MediaPlaylist, MediaRendition, MediaSegment, VariantStream,
    attribute::Attributes,
    lexical::{collect_lines, parse_tag, validate_text},
    master::{
        SessionKeyIdentity, VariantFields, parse_rendition, parse_variant, session_key_identity,
        validate_i_frame_variant, validate_master_relations, validate_session_data,
    },
    media::{parse_byte_range, parse_key, parse_map},
    structure::validate_target_duration,
};

/// Запрос pure parser с опциональной base для проверки URI-reference.
#[derive(Clone, Copy, Debug)]
pub struct HlsParseRequest<'a> {
    /// Недоверенные bytes manifest.
    pub document_bytes: &'a [u8],
    /// Absolute base, используемая только для проверки resolution-ready references.
    pub reference_base: Option<&'a str>,
    /// Явные work/allocation budgets.
    pub limits: HlsParserLimits,
}

impl<'a> HlsParseRequest<'a> {
    /// Создаёт явный parse request.
    pub const fn new(
        document_bytes: &'a [u8],
        reference_base: Option<&'a str>,
        limits: HlsParserLimits,
    ) -> Self {
        Self {
            document_bytes,
            reference_base,
            limits,
        }
    }
}

/// Content-first поиск HLS marker до интерпретации как generic M3U.
pub fn is_hls_candidate(text_without_bom: &str) -> bool {
    text_without_bom.lines().any(|line| {
        line.get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXT-X-"))
    })
}

/// Разбирает bounded RFC 8216 document во владеющую master/media model.
pub fn parse_hls_playlist(request: HlsParseRequest<'_>) -> Result<HlsPlaylist, HlsParseError> {
    if request.document_bytes.len() > request.limits.max_document_bytes() {
        return Err(HlsParseError::new(HlsParseErrorKind::DocumentLimitExceeded));
    }
    let text = std::str::from_utf8(request.document_bytes)
        .map_err(|_| HlsParseError::new(HlsParseErrorKind::InvalidUtf8))?;
    if text.starts_with('\u{feff}') {
        return Err(HlsParseError::new(HlsParseErrorKind::BomNotAllowed));
    }
    validate_text(text)?;
    let lines = collect_lines(text, request.limits)?;
    if lines.first().map(|line| line.text) != Some("#EXTM3U") {
        return Err(HlsParseError::new(HlsParseErrorKind::MissingHeader));
    }
    Parser::new(lines, request.reference_base, request.limits).parse()
}

#[derive(Clone, Copy)]
pub(super) struct Line<'a> {
    pub(super) number: HlsLineNumber,
    pub(super) text: &'a str,
}

pub(super) struct Tag<'a> {
    pub(super) name: &'a str,
    pub(super) value: Option<&'a str>,
    pub(super) line: HlsLineNumber,
}

#[derive(Default)]
struct Topology {
    master: bool,
    media: bool,
}

struct Parser<'a> {
    lines: Vec<Line<'a>>,
    base: Option<Url>,
    limits: HlsParserLimits,
    singleton_tags: HashSet<&'a str>,
    topology: Topology,
    variants: Vec<VariantStream>,
    variant_lines: Vec<HlsLineNumber>,
    renditions: Vec<MediaRendition>,
    rendition_lines: Vec<HlsLineNumber>,
    segments: Vec<MediaSegment>,
    segment_lines: Vec<HlsLineNumber>,
    protocol_version: Option<u64>,
    target_duration: Option<u64>,
    initial_media_sequence: u64,
    next_media_sequence: u64,
    end_list: bool,
    active_key: Option<HlsKeyDeclaration>,
    next_key_declaration_sequence: u64,
    key_declarations: Vec<HlsKeyDeclaration>,
    session_keys: Vec<HlsKeyDeclaration>,
    active_map: Option<InitializationMap>,
    pending_duration: Option<(HlsDuration, Box<str>, HlsLineNumber)>,
    pending_variant: Option<(VariantFields, HlsLineNumber)>,
    pending_range: Option<(ByteRange, HlsLineNumber)>,
    pending_discontinuity: Option<HlsLineNumber>,
    saw_discontinuity: bool,
    low_latency: bool,
    i_frames_only: bool,
    playlist_type: Option<HlsPlaylistType>,
    session_key: bool,
    session_key_identities: HashSet<SessionKeyIdentity<'a>>,
    session_data_identities: HashSet<(&'a str, Option<&'a str>)>,
    inline_master_reference: bool,
    has_i_frame_variant: bool,
    has_start_offset: bool,
    has_variable_substitution: bool,
    has_content_steering: bool,
}

impl<'a> Parser<'a> {
    fn new(lines: Vec<Line<'a>>, base: Option<&str>, limits: HlsParserLimits) -> Self {
        Self {
            lines,
            base: base.and_then(|base| Url::parse(base).ok()),
            limits,
            singleton_tags: HashSet::from(["#EXTM3U"]),
            topology: Topology::default(),
            variants: Vec::new(),
            variant_lines: Vec::new(),
            renditions: Vec::new(),
            rendition_lines: Vec::new(),
            segments: Vec::new(),
            segment_lines: Vec::new(),
            protocol_version: None,
            target_duration: None,
            initial_media_sequence: 0,
            next_media_sequence: 0,
            end_list: false,
            active_key: None,
            next_key_declaration_sequence: 0,
            key_declarations: Vec::new(),
            session_keys: Vec::new(),
            active_map: None,
            pending_duration: None,
            pending_variant: None,
            pending_range: None,
            pending_discontinuity: None,
            saw_discontinuity: false,
            low_latency: false,
            i_frames_only: false,
            playlist_type: None,
            session_key: false,
            session_key_identities: HashSet::new(),
            session_data_identities: HashSet::new(),
            inline_master_reference: false,
            has_i_frame_variant: false,
            has_start_offset: false,
            has_variable_substitution: false,
            has_content_steering: false,
        }
    }

    fn parse(mut self) -> Result<HlsPlaylist, HlsParseError> {
        for index in 1..self.lines.len() {
            let line = self.lines[index];
            if line.text.is_empty() {
                continue;
            }
            if line.text.starts_with('#') {
                if line
                    .text
                    .get(..4)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXT"))
                {
                    let tag = parse_tag(line)?;
                    self.handle_tag(tag)?;
                }
            } else {
                self.handle_uri(line)?;
            }
        }
        if let Some((_, _, line)) = self.pending_duration.take() {
            return Err(required(line));
        }
        if let Some((_, line)) = self.pending_variant.take() {
            return Err(required(line));
        }
        if let Some((_, line)) = self.pending_range.take() {
            return Err(required(line));
        }
        if let Some(line) = self.pending_discontinuity.take() {
            return Err(required(line));
        }
        if self.topology.master && self.topology.media {
            return Err(HlsParseError::new(HlsParseErrorKind::MixedTopology));
        }
        if self.topology.master {
            if self.variants.is_empty() && !self.inline_master_reference {
                return Err(required(HlsLineNumber::from_index(0)));
            }
            validate_master_relations(
                &self.variants,
                &self.variant_lines,
                &self.renditions,
                &self.rendition_lines,
            )?;
            return Ok(HlsPlaylist::Master(MasterPlaylist {
                variants: self.variants.into_boxed_slice(),
                renditions: self.renditions.into_boxed_slice(),
                has_session_key: self.session_key,
                session_keys: self.session_keys.into_boxed_slice(),
                has_low_latency_semantics: self.low_latency,
                protocol_version: self.protocol_version,
                has_i_frame_variant: self.has_i_frame_variant,
                has_start_offset: self.has_start_offset,
                has_variable_substitution: self.has_variable_substitution,
                has_content_steering: self.has_content_steering,
            }));
        }
        if self.topology.media {
            let target_duration_seconds = self
                .target_duration
                .ok_or_else(|| required(HlsLineNumber::from_index(0)))?;
            validate_target_duration(target_duration_seconds, &self.segments, &self.segment_lines)?;
            return Ok(HlsPlaylist::Media(MediaPlaylist {
                target_duration_seconds,
                media_sequence: self.initial_media_sequence,
                segments: self.segments.into_boxed_slice(),
                key_declarations: self.key_declarations.into_boxed_slice(),
                end_list: self.end_list,
                has_low_latency_semantics: self.low_latency,
                i_frames_only: self.i_frames_only,
                playlist_type: self.playlist_type,
                protocol_version: self.protocol_version,
                has_start_offset: self.has_start_offset,
                has_variable_substitution: self.has_variable_substitution,
                has_content_steering: self.has_content_steering,
            }));
        }
        Err(HlsParseError::new(HlsParseErrorKind::UnknownTopology))
    }

    fn handle_tag(&mut self, tag: Tag<'a>) -> Result<(), HlsParseError> {
        if singleton(tag.name) && !self.singleton_tags.insert(tag.name) {
            return Err(HlsParseError::new(HlsParseErrorKind::DuplicateTag {
                line: tag.line,
            }));
        }
        record_topology(tag.name, &mut self.topology);
        if known_low_latency(tag.name) {
            self.low_latency = true;
        }
        match tag.name {
            "#EXT-X-VERSION" => {
                let version = parse_u64(required_value(&tag)?, tag.line)?;
                if version == 0 {
                    return Err(syntax(tag.line));
                }
                self.protocol_version = Some(version);
            }
            "#EXTINF" => {
                if self.pending_duration.is_some() {
                    return Err(required(tag.line));
                }
                let value = required_value(&tag)?;
                let (duration, title) = value.split_once(',').ok_or_else(|| syntax(tag.line))?;
                validate_decimal_duration(duration).ok_or_else(|| syntax(tag.line))?;
                self.pending_duration = Some((HlsDuration::new(duration), title.into(), tag.line));
            }
            "#EXT-X-STREAM-INF" => {
                if self.pending_variant.is_some() {
                    return Err(required(tag.line));
                }
                let attributes = self.attributes(&tag)?;
                self.pending_variant = Some((parse_variant(&attributes, tag.line)?, tag.line));
            }
            "#EXT-X-MEDIA" => {
                let attributes = self.attributes(&tag)?;
                let rendition = parse_rendition(&attributes, tag.line, self.base.as_ref())?;
                if self.renditions.len() >= self.limits.max_renditions() {
                    return Err(HlsParseError::new(
                        HlsParseErrorKind::RenditionLimitExceeded,
                    ));
                }
                self.renditions.push(rendition);
                self.rendition_lines.push(tag.line);
            }
            "#EXT-X-TARGETDURATION" => {
                let target_duration = parse_u64(required_value(&tag)?, tag.line)?;
                if target_duration == 0 {
                    return Err(required(tag.line));
                }
                self.target_duration = Some(target_duration);
            }
            "#EXT-X-MEDIA-SEQUENCE" => {
                if !self.segments.is_empty() {
                    return Err(required(tag.line));
                }
                let sequence = parse_u64(required_value(&tag)?, tag.line)?;
                self.initial_media_sequence = sequence;
                self.next_media_sequence = sequence;
            }
            "#EXT-X-ENDLIST" => {
                require_no_value(&tag)?;
                self.end_list = true;
            }
            "#EXT-X-BYTERANGE" => {
                if self.pending_range.is_some() {
                    return Err(required(tag.line));
                }
                self.pending_range =
                    Some((parse_byte_range(required_value(&tag)?, tag.line)?, tag.line));
            }
            "#EXT-X-DISCONTINUITY" => {
                require_no_value(&tag)?;
                if self.pending_discontinuity.is_some() {
                    return Err(required(tag.line));
                }
                self.pending_discontinuity = Some(tag.line);
                self.saw_discontinuity = true;
            }
            "#EXT-X-DISCONTINUITY-SEQUENCE" => {
                if !self.segments.is_empty() || self.saw_discontinuity {
                    return Err(required(tag.line));
                }
                parse_u64(required_value(&tag)?, tag.line)?;
            }
            "#EXT-X-KEY" => {
                let attributes = self.attributes(&tag)?;
                let declaration_sequence = self.next_key_declaration_sequence;
                self.next_key_declaration_sequence = declaration_sequence
                    .checked_add(1)
                    .ok_or_else(|| required(tag.line))?;
                let key = parse_key(
                    &attributes,
                    tag.line,
                    self.base.as_ref(),
                    declaration_sequence,
                )?;
                self.active_key =
                    (!matches!(key.method, HlsKeyMethod::None)).then_some(key.clone());
                self.key_declarations.push(key);
            }
            "#EXT-X-MAP" => {
                let attributes = self.attributes(&tag)?;
                self.active_map = Some(parse_map(
                    &attributes,
                    tag.line,
                    self.base.as_ref(),
                    self.active_key.clone(),
                )?);
            }
            "#EXT-X-I-FRAMES-ONLY" => {
                require_no_value(&tag)?;
                self.i_frames_only = true;
            }
            "#EXT-X-PLAYLIST-TYPE" => {
                self.playlist_type = Some(match required_value(&tag)? {
                    "EVENT" => HlsPlaylistType::Event,
                    "VOD" => HlsPlaylistType::Vod,
                    _ => return Err(syntax(tag.line)),
                });
            }
            "#EXT-X-SESSION-KEY" => {
                let attributes = self.attributes(&tag)?;
                let declaration_sequence = self.next_key_declaration_sequence;
                self.next_key_declaration_sequence = declaration_sequence
                    .checked_add(1)
                    .ok_or_else(|| required(tag.line))?;
                let key = parse_key(
                    &attributes,
                    tag.line,
                    self.base.as_ref(),
                    declaration_sequence,
                )?;
                if matches!(key.method, HlsKeyMethod::None) {
                    return Err(required(tag.line));
                }
                let identity = session_key_identity(&attributes, tag.line)?;
                if !self.session_key_identities.insert(identity) {
                    return Err(required(tag.line));
                }
                self.session_keys.push(key);
                self.session_key = true;
            }
            "#EXT-X-I-FRAME-STREAM-INF" => {
                let attributes = self.attributes(&tag)?;
                validate_i_frame_variant(&attributes, tag.line, self.base.as_ref())?;
                self.inline_master_reference = true;
                self.has_i_frame_variant = true;
            }
            "#EXT-X-SESSION-DATA" => {
                let attributes = self.attributes(&tag)?;
                let identity = validate_session_data(&attributes, tag.line, self.base.as_ref())?;
                if !self.session_data_identities.insert(identity) {
                    return Err(required(tag.line));
                }
            }
            "#EXT-X-INDEPENDENT-SEGMENTS" => {
                // Это только положительная decode-independence гарантия; игнорировать её безопасно.
                require_no_value(&tag)?;
            }
            "#EXT-X-START" => {
                let attributes = self.attributes(&tag)?;
                validate_start(&attributes, tag.line)?;
                self.has_start_offset = true;
            }
            "#EXT-X-PROGRAM-DATE-TIME" => {
                // Wall-clock mapping не меняет relative VOD timeline или media bytes.
                validate_non_empty_generic_value(&tag)?;
            }
            "#EXT-X-DATERANGE" => {
                // Timed metadata не участвует в S32 playback; required shape всё равно проверяется.
                let attributes = self.attributes(&tag)?;
                validate_date_range(&attributes, tag.line)?;
            }
            "#EXT-X-DEFINE" => {
                self.attributes(&tag)?;
                self.has_variable_substitution = true;
            }
            "#EXT-X-CONTENT-STEERING" => {
                self.attributes(&tag)?;
                self.has_content_steering = true;
            }
            name if attribute_list_tag(name) => {
                self.attributes(&tag)?;
            }
            _ => validate_generic_value(&tag)?,
        }
        Ok(())
    }

    fn attributes(&self, tag: &Tag<'a>) -> Result<Attributes<'a>, HlsParseError> {
        Attributes::parse(required_value(tag)?, tag.line, self.limits)
    }

    fn handle_uri(&mut self, line: Line<'a>) -> Result<(), HlsParseError> {
        if line.text.chars().any(char::is_whitespace) {
            return Err(HlsParseError::new(
                HlsParseErrorKind::WhitespaceNotAllowed { line: line.number },
            ));
        }
        validate_reference(line.text, line.number, self.base.as_ref())?;
        if let Some((fields, variant_line)) = self.pending_variant.take() {
            if self.variants.len() >= self.limits.max_variants() {
                return Err(HlsParseError::new(HlsParseErrorKind::VariantLimitExceeded));
            }
            self.variants
                .push(fields.finish(ExactReference::new(line.text)));
            self.variant_lines.push(variant_line);
            return Ok(());
        }
        let Some((duration, title, duration_line)) = self.pending_duration.take() else {
            return Err(required(line.number));
        };
        if self.segments.len() >= self.limits.max_segments() {
            return Err(HlsParseError::new(HlsParseErrorKind::SegmentLimitExceeded));
        }
        self.segments.push(MediaSegment {
            uri: ExactReference::new(line.text),
            duration,
            title,
            byte_range: self.pending_range.take().map(|(range, _)| range),
            discontinuity: self.pending_discontinuity.take().is_some(),
            media_sequence: self.next_media_sequence,
            initialization_map: self.active_map.clone(),
            key: self.active_key.clone(),
        });
        self.segment_lines.push(duration_line);
        self.next_media_sequence = self
            .next_media_sequence
            .checked_add(1)
            .ok_or_else(|| syntax(line.number))?;
        Ok(())
    }
}

fn record_topology(name: &str, topology: &mut Topology) {
    topology.master |= matches!(
        name,
        "#EXT-X-MEDIA"
            | "#EXT-X-STREAM-INF"
            | "#EXT-X-I-FRAME-STREAM-INF"
            | "#EXT-X-SESSION-DATA"
            | "#EXT-X-SESSION-KEY"
    );
    topology.media |= name == "#EXTINF"
        || matches!(
            name,
            "#EXT-X-BYTERANGE"
                | "#EXT-X-DISCONTINUITY"
                | "#EXT-X-KEY"
                | "#EXT-X-MAP"
                | "#EXT-X-PROGRAM-DATE-TIME"
                | "#EXT-X-DATERANGE"
                | "#EXT-X-TARGETDURATION"
                | "#EXT-X-MEDIA-SEQUENCE"
                | "#EXT-X-DISCONTINUITY-SEQUENCE"
                | "#EXT-X-ENDLIST"
                | "#EXT-X-PLAYLIST-TYPE"
                | "#EXT-X-I-FRAMES-ONLY"
        );
}

fn singleton(name: &str) -> bool {
    matches!(
        name,
        "#EXTM3U"
            | "#EXT-X-VERSION"
            | "#EXT-X-TARGETDURATION"
            | "#EXT-X-MEDIA-SEQUENCE"
            | "#EXT-X-DISCONTINUITY-SEQUENCE"
            | "#EXT-X-ENDLIST"
            | "#EXT-X-PLAYLIST-TYPE"
            | "#EXT-X-I-FRAMES-ONLY"
            | "#EXT-X-INDEPENDENT-SEGMENTS"
            | "#EXT-X-START"
    )
}

fn known_low_latency(name: &str) -> bool {
    matches!(
        name,
        "#EXT-X-PART"
            | "#EXT-X-PART-INF"
            | "#EXT-X-SERVER-CONTROL"
            | "#EXT-X-SKIP"
            | "#EXT-X-PRELOAD-HINT"
            | "#EXT-X-RENDITION-REPORT"
    )
}

fn attribute_list_tag(name: &str) -> bool {
    matches!(
        name,
        "#EXT-X-I-FRAME-STREAM-INF"
            | "#EXT-X-SESSION-DATA"
            | "#EXT-X-DATERANGE"
            | "#EXT-X-START"
            | "#EXT-X-DEFINE"
            | "#EXT-X-CONTENT-STEERING"
            | "#EXT-X-PART"
            | "#EXT-X-PART-INF"
            | "#EXT-X-PRELOAD-HINT"
            | "#EXT-X-RENDITION-REPORT"
            | "#EXT-X-SERVER-CONTROL"
            | "#EXT-X-SKIP"
    )
}

pub(crate) fn validate_reference(
    reference: &str,
    line: HlsLineNumber,
    base: Option<&Url>,
) -> Result<(), HlsParseError> {
    if reference.is_empty() || reference.chars().any(char::is_whitespace) {
        return Err(HlsParseError::new(HlsParseErrorKind::InvalidReference {
            line,
        }));
    }
    let valid = Url::parse(reference)
        .map(|absolute| !absolute.cannot_be_a_base())
        .unwrap_or_else(|_| base.is_none_or(|base| base.join(reference).is_ok()));
    if !valid {
        return Err(HlsParseError::new(HlsParseErrorKind::InvalidReference {
            line,
        }));
    }
    Ok(())
}

fn validate_decimal_duration(value: &str) -> Option<()> {
    let (whole, fraction) = value
        .split_once('.')
        .map_or((value, None), |(whole, fraction)| (whole, Some(fraction)));
    (!whole.is_empty()
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.is_none_or(|fraction| {
            !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit())
        }))
    .then_some(())
}

fn validate_signed_decimal_duration(value: &str) -> Option<()> {
    validate_decimal_duration(value.strip_prefix('-').unwrap_or(value))
}

fn validate_start(attributes: &Attributes<'_>, line: HlsLineNumber) -> Result<(), HlsParseError> {
    let time_offset = attributes
        .raw("TIME-OFFSET")
        .ok_or_else(|| required(line))?;
    validate_signed_decimal_duration(time_offset).ok_or_else(|| syntax(line))?;
    if let Some(precise) = attributes.raw("PRECISE")
        && !matches!(precise, "YES" | "NO")
    {
        return Err(syntax(line));
    }
    Ok(())
}

fn validate_date_range(
    attributes: &Attributes<'_>,
    line: HlsLineNumber,
) -> Result<(), HlsParseError> {
    attributes.quoted("ID").ok_or_else(|| required(line))?;
    attributes
        .quoted("START-DATE")
        .ok_or_else(|| required(line))?;
    if let Some(duration) = attributes.raw("DURATION") {
        validate_decimal_duration(duration).ok_or_else(|| syntax(line))?;
    }
    if let Some(duration) = attributes.raw("PLANNED-DURATION") {
        validate_decimal_duration(duration).ok_or_else(|| syntax(line))?;
    }
    if let Some(end_on_next) = attributes.raw("END-ON-NEXT") {
        if end_on_next != "YES" || attributes.quoted("CLASS").is_none() {
            return Err(required(line));
        }
        if attributes.raw("DURATION").is_some() || attributes.raw("END-DATE").is_some() {
            return Err(required(line));
        }
    }
    Ok(())
}

fn validate_non_empty_generic_value(tag: &Tag<'_>) -> Result<(), HlsParseError> {
    let value = required_value(tag)?;
    if value.is_empty() {
        return Err(syntax(tag.line));
    }
    validate_generic_value(tag)
}

pub(crate) fn parse_u64(value: &str, line: HlsLineNumber) -> Result<u64, HlsParseError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(syntax(line));
    }
    value.parse().map_err(|_| syntax(line))
}

fn required_value<'a>(tag: &Tag<'a>) -> Result<&'a str, HlsParseError> {
    tag.value.ok_or_else(|| syntax(tag.line))
}

fn require_no_value(tag: &Tag<'_>) -> Result<(), HlsParseError> {
    if tag.value.is_some() {
        return Err(syntax(tag.line));
    }
    Ok(())
}

fn validate_generic_value(tag: &Tag<'_>) -> Result<(), HlsParseError> {
    if tag
        .value
        .is_some_and(|value| value.chars().any(char::is_whitespace))
    {
        return Err(HlsParseError::new(
            HlsParseErrorKind::WhitespaceNotAllowed { line: tag.line },
        ));
    }
    Ok(())
}

fn syntax(line: HlsLineNumber) -> HlsParseError {
    HlsParseError::new(HlsParseErrorKind::InvalidTagSyntax { line })
}

fn required(line: HlsLineNumber) -> HlsParseError {
    HlsParseError::new(HlsParseErrorKind::InvalidRequiredStructure { line })
}
