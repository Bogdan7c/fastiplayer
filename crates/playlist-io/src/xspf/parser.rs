//! Streaming XSPF v1 schema state machine поверх `BoundedXmlReader`.

use bounded_xml_reader::{BoundedXmlReader, XmlElement, XmlEvent, XmlExpandedName};
use media_core::{MediaDuration, TrackNumber};
use url::Url;

use crate::XspfDocumentSource;

use super::XSPF_NAMESPACE;
use super::error::{XspfParseError, XspfParseErrorKind};
use super::extension::{parse_playlist_extension, parse_track_extension, validate_group_ranges};
use super::limits::XspfParserLimits;
use super::model::{XspfGroup, XspfLocationCandidate, XspfPlaylist, XspfTrack};
use super::schema::{ChildCardinality, ChildSequence, parse_duration_hint, parse_track_number};
use super::uri::{document_base, matches_xml_whitespace, resolve_element_base, resolve_location};

/// Reserved XML namespace URI для implicit `xml:*` attributes.
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

/// Self-documenting XSPF request без hidden I/O или limits.
pub struct XspfParseRequest<'document> {
    /// Caller-owned UTF-8 XML bytes.
    document_bytes: &'document [u8],
    /// Retrieval/local identity определяет initial XML Base.
    source: XspfDocumentSource,
    /// Explicit XSPF и XML budgets.
    limits: XspfParserLimits,
}

impl<'document> XspfParseRequest<'document> {
    /// Собирает complete parse intent.
    pub fn new(
        document_bytes: &'document [u8],
        source: XspfDocumentSource,
        limits: XspfParserLimits,
    ) -> Self {
        Self {
            document_bytes,
            source,
            limits,
        }
    }
}

/// Единственная XSPF v1 import entry point.
pub fn parse_xspf_document(request: XspfParseRequest<'_>) -> Result<XspfPlaylist, XspfParseError> {
    let XspfParseRequest {
        document_bytes,
        source,
        limits,
    } = request;

    // Preview не обещает больше tracks, чем будущая atomic queue transaction может принять.
    if limits.maximum_tracks() > playlist_core::MAX_PLAYLIST_ITEMS {
        return Err(XspfParseError::new(
            XspfParseErrorKind::TrackLimitExceedsDomainCapacity,
        ));
    }
    // Group records также не получают бессмысленный cap выше domain capacity.
    if limits.maximum_groups() > playlist_core::MAX_PLAYLIST_ITEMS {
        return Err(XspfParseError::new(
            XspfParseErrorKind::GroupLimitExceedsDomainCapacity,
        ));
    }

    // Base строится до XML traversal и не выполняет filesystem access.
    let initial_base = document_base(&source)?;
    // Hardened boundary получает complete caller-defined XML profile.
    let reader = BoundedXmlReader::new(document_bytes, limits.xml_budgets())
        .map_err(XspfParseError::from_xml)?;
    // Cursor централизует terminal XML error mapping.
    let mut cursor = EventCursor { reader };

    // Root обязан быть обычным non-empty XSPF playlist element.
    let root_element = match cursor.next_event()? {
        Some(XmlEvent::StartElement(element)) => element,
        Some(XmlEvent::EmptyElement(_))
        | Some(XmlEvent::EndElement(_))
        | Some(XmlEvent::Text(_))
        | None => return Err(XspfParseError::new(XspfParseErrorKind::InvalidRoot)),
    };

    // Schema parser materialize-ит tracks/groups только внутри format caps.
    let playlist = parse_playlist(&mut cursor, root_element, &initial_base, limits)?;
    // Root parser обязан consume exact closing element и validated EOF.
    if cursor.next_event()?.is_some() {
        return Err(XspfParseError::new(XspfParseErrorKind::InvalidRoot));
    }
    Ok(playlist)
}

/// Reader adapter не прячет XML distinctions за общим malformed outcome.
pub(super) struct EventCursor<'document> {
    /// Security boundary остаётся единственным concrete tokenizer owner-ом.
    reader: BoundedXmlReader<'document>,
}

impl EventCursor<'_> {
    /// Возвращает следующий materialized event.
    pub(super) fn next_event(&mut self) -> Result<Option<XmlEvent>, XspfParseError> {
        self.reader.next_event().map_err(XspfParseError::from_xml)
    }

    /// Пропускает только legal formatting whitespace внутри container elements.
    pub(super) fn next_container_event(&mut self) -> Result<Option<XmlEvent>, XspfParseError> {
        loop {
            match self.next_event()? {
                Some(XmlEvent::Text(text))
                    if text.content().chars().all(matches_xml_whitespace) => {}
                next_event => return Ok(next_event),
            }
        }
    }
}

/// Парсит root children с exact order/cardinality и mandatory trackList.
fn parse_playlist(
    cursor: &mut EventCursor<'_>,
    root: XmlElement,
    document_base: &Url,
    limits: XspfParserLimits,
) -> Result<XspfPlaylist, XspfParseError> {
    require_name(root.name(), XSPF_NAMESPACE, "playlist")?;
    validate_attributes(&root, &["version"])?;
    let version = required_unqualified_attribute(&root, "version")?;
    if version != "1" {
        return Err(XspfParseError::new(XspfParseErrorKind::UnsupportedVersion));
    }
    let playlist_base = element_base(&root, document_base)?;
    let mut sequence = ChildSequence::default();
    let mut tracks = Vec::new();
    let mut groups = Vec::new();
    let mut track_list_seen = false;
    let mut rustiplayer_extension_seen = false;

    loop {
        match cursor.next_container_event()? {
            Some(XmlEvent::StartElement(element)) => {
                parse_playlist_child(
                    cursor,
                    element,
                    false,
                    &playlist_base,
                    limits,
                    &mut sequence,
                    &mut tracks,
                    &mut groups,
                    &mut track_list_seen,
                    &mut rustiplayer_extension_seen,
                )?;
            }
            Some(XmlEvent::EmptyElement(element)) => {
                parse_playlist_child(
                    cursor,
                    element,
                    true,
                    &playlist_base,
                    limits,
                    &mut sequence,
                    &mut tracks,
                    &mut groups,
                    &mut track_list_seen,
                    &mut rustiplayer_extension_seen,
                )?;
            }
            Some(XmlEvent::EndElement(name)) => {
                require_name(&name, XSPF_NAMESPACE, "playlist")?;
                break;
            }
            Some(XmlEvent::Text(_)) => {
                return Err(XspfParseError::new(XspfParseErrorKind::TextNotAllowed));
            }
            None => return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedEof)),
        }
    }

    if !track_list_seen {
        return Err(XspfParseError::new(XspfParseErrorKind::MissingTrackList));
    }
    validate_group_ranges(&groups, tracks.len())?;
    Ok(XspfPlaylist::new(tracks, groups))
}

/// Dispatch root child без переноса schema knowledge в XML boundary.
#[allow(clippy::too_many_arguments)]
fn parse_playlist_child(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
    limits: XspfParserLimits,
    sequence: &mut ChildSequence,
    tracks: &mut Vec<XspfTrack>,
    groups: &mut Vec<XspfGroup>,
    track_list_seen: &mut bool,
    rustiplayer_extension_seen: &mut bool,
) -> Result<(), XspfParseError> {
    require_namespace(element.name(), XSPF_NAMESPACE)?;
    match element.name().local_name() {
        "title" => parse_ignored_text(cursor, element, is_empty, parent_base, sequence, 0),
        "creator" => parse_ignored_text(cursor, element, is_empty, parent_base, sequence, 1),
        "annotation" => parse_ignored_text(cursor, element, is_empty, parent_base, sequence, 2),
        "info" | "location" | "identifier" | "image" | "license" => {
            let rank = match element.name().local_name() {
                "info" => 3,
                "location" => 4,
                "identifier" => 5,
                "image" => 6,
                "license" => 8,
                _ => unreachable!("matched URI child"),
            };
            parse_ignored_uri(cursor, element, is_empty, parent_base, sequence, rank)
        }
        "date" => parse_ignored_text(cursor, element, is_empty, parent_base, sequence, 7),
        "attribution" => {
            sequence.accept(9, ChildCardinality::Optional)?;
            parse_attribution(cursor, element, is_empty, parent_base)
        }
        "link" => {
            sequence.accept(10, ChildCardinality::Repeated)?;
            parse_link(cursor, element, is_empty, parent_base)
        }
        "meta" => {
            sequence.accept(11, ChildCardinality::Repeated)?;
            parse_meta(cursor, element, is_empty, parent_base)
        }
        "extension" => {
            sequence.accept(12, ChildCardinality::Repeated)?;
            parse_playlist_extension(
                cursor,
                element,
                is_empty,
                parent_base,
                limits,
                groups,
                rustiplayer_extension_seen,
            )
        }
        "trackList" => {
            sequence.accept(13, ChildCardinality::Optional)?;
            *track_list_seen = true;
            parse_track_list(cursor, element, is_empty, parent_base, limits, tracks)
        }
        _ => Err(XspfParseError::new(XspfParseErrorKind::UnexpectedElement)),
    }
}

/// Парсит ordered track list; empty list является valid XSPF.
fn parse_track_list(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
    limits: XspfParserLimits,
    tracks: &mut Vec<XspfTrack>,
) -> Result<(), XspfParseError> {
    validate_attributes(&element, &[])?;
    let track_list_base = element_base(&element, parent_base)?;
    if is_empty {
        return Ok(());
    }

    loop {
        match cursor.next_container_event()? {
            Some(XmlEvent::StartElement(track_element)) => {
                require_name(track_element.name(), XSPF_NAMESPACE, "track")?;
                push_track(
                    tracks,
                    parse_track(cursor, track_element, false, &track_list_base, limits)?,
                    limits,
                )?;
            }
            Some(XmlEvent::EmptyElement(track_element)) => {
                require_name(track_element.name(), XSPF_NAMESPACE, "track")?;
                push_track(
                    tracks,
                    parse_track(cursor, track_element, true, &track_list_base, limits)?,
                    limits,
                )?;
            }
            Some(XmlEvent::EndElement(name)) => {
                require_name(&name, XSPF_NAMESPACE, "trackList")?;
                return Ok(());
            }
            Some(XmlEvent::Text(_)) => {
                return Err(XspfParseError::new(XspfParseErrorKind::TextNotAllowed));
            }
            None => return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedEof)),
        }
    }
}

/// Проверяет track cap до Vec growth.
fn push_track(
    tracks: &mut Vec<XspfTrack>,
    track: XspfTrack,
    limits: XspfParserLimits,
) -> Result<(), XspfParseError> {
    if tracks.len() >= limits.maximum_tracks() {
        return Err(XspfParseError::new(XspfParseErrorKind::TrackLimitExceeded));
    }
    tracks.push(track);
    Ok(())
}

/// Парсит один track и сохраняет только S06 hints.
fn parse_track(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
    limits: XspfParserLimits,
) -> Result<XspfTrack, XspfParseError> {
    validate_attributes(&element, &[])?;
    let track_base = element_base(&element, parent_base)?;
    let mut sequence = ChildSequence::default();
    let mut locations = Vec::new();
    let mut title = None;
    let mut creator = None;
    let mut album = None;
    let mut track_number = None;
    let mut duration_hint = None;

    if is_empty {
        return Ok(XspfTrack::new(
            locations,
            title,
            creator,
            album,
            track_number,
            duration_hint,
        ));
    }

    loop {
        match cursor.next_container_event()? {
            Some(XmlEvent::StartElement(child)) => {
                parse_track_child(
                    cursor,
                    child,
                    false,
                    &track_base,
                    limits,
                    &mut sequence,
                    &mut locations,
                    &mut title,
                    &mut creator,
                    &mut album,
                    &mut track_number,
                    &mut duration_hint,
                )?;
            }
            Some(XmlEvent::EmptyElement(child)) => {
                parse_track_child(
                    cursor,
                    child,
                    true,
                    &track_base,
                    limits,
                    &mut sequence,
                    &mut locations,
                    &mut title,
                    &mut creator,
                    &mut album,
                    &mut track_number,
                    &mut duration_hint,
                )?;
            }
            Some(XmlEvent::EndElement(name)) => {
                require_name(&name, XSPF_NAMESPACE, "track")?;
                break;
            }
            Some(XmlEvent::Text(_)) => {
                return Err(XspfParseError::new(XspfParseErrorKind::TextNotAllowed));
            }
            None => return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedEof)),
        }
    }

    Ok(XspfTrack::new(
        locations,
        title,
        creator,
        album,
        track_number,
        duration_hint,
    ))
}

/// Dispatch track child с exact spec order/cardinality.
#[allow(clippy::too_many_arguments)]
fn parse_track_child(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
    limits: XspfParserLimits,
    sequence: &mut ChildSequence,
    locations: &mut Vec<XspfLocationCandidate>,
    title: &mut Option<String>,
    creator: &mut Option<String>,
    album: &mut Option<String>,
    track_number: &mut Option<TrackNumber>,
    duration_hint: &mut Option<MediaDuration>,
) -> Result<(), XspfParseError> {
    require_namespace(element.name(), XSPF_NAMESPACE)?;
    match element.name().local_name() {
        "location" => {
            sequence.accept(0, ChildCardinality::Repeated)?;
            if locations.len() >= limits.maximum_locations_per_track() {
                return Err(XspfParseError::new(
                    XspfParseErrorKind::LocationLimitExceeded,
                ));
            }
            locations.push(parse_location(cursor, element, is_empty, parent_base)?);
            Ok(())
        }
        "identifier" => {
            sequence.accept(1, ChildCardinality::Repeated)?;
            parse_uri_value(cursor, element, is_empty, parent_base).map(|_| ())
        }
        "title" => {
            sequence.accept(2, ChildCardinality::Optional)?;
            *title = Some(parse_text_value(cursor, element, is_empty, parent_base)?);
            Ok(())
        }
        "creator" => {
            sequence.accept(3, ChildCardinality::Optional)?;
            *creator = Some(parse_text_value(cursor, element, is_empty, parent_base)?);
            Ok(())
        }
        "annotation" => {
            sequence.accept(4, ChildCardinality::Optional)?;
            parse_text_value(cursor, element, is_empty, parent_base).map(|_| ())
        }
        "info" => {
            sequence.accept(5, ChildCardinality::Optional)?;
            parse_uri_value(cursor, element, is_empty, parent_base).map(|_| ())
        }
        "image" => {
            sequence.accept(6, ChildCardinality::Optional)?;
            parse_uri_value(cursor, element, is_empty, parent_base).map(|_| ())
        }
        "album" => {
            sequence.accept(7, ChildCardinality::Optional)?;
            *album = Some(parse_text_value(cursor, element, is_empty, parent_base)?);
            Ok(())
        }
        "trackNum" => {
            sequence.accept(8, ChildCardinality::Optional)?;
            let text = parse_text_value(cursor, element, is_empty, parent_base)?;
            *track_number = Some(parse_track_number(&text)?);
            Ok(())
        }
        "duration" => {
            sequence.accept(9, ChildCardinality::Optional)?;
            let text = parse_text_value(cursor, element, is_empty, parent_base)?;
            *duration_hint = Some(parse_duration_hint(&text)?);
            Ok(())
        }
        "link" => {
            sequence.accept(10, ChildCardinality::Repeated)?;
            parse_link(cursor, element, is_empty, parent_base)
        }
        "meta" => {
            sequence.accept(11, ChildCardinality::Repeated)?;
            parse_meta(cursor, element, is_empty, parent_base)
        }
        "extension" => {
            sequence.accept(12, ChildCardinality::Repeated)?;
            parse_track_extension(cursor, element, is_empty, parent_base)
        }
        _ => Err(XspfParseError::new(XspfParseErrorKind::UnexpectedElement)),
    }
}

/// Attribution child order: location* затем identifier*.
fn parse_attribution(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
) -> Result<(), XspfParseError> {
    validate_attributes(&element, &[])?;
    let attribution_base = element_base(&element, parent_base)?;
    if is_empty {
        return Ok(());
    }
    let mut sequence = ChildSequence::default();
    loop {
        match cursor.next_container_event()? {
            Some(XmlEvent::StartElement(child)) => {
                parse_attribution_child(cursor, child, false, &attribution_base, &mut sequence)?;
            }
            Some(XmlEvent::EmptyElement(child)) => {
                parse_attribution_child(cursor, child, true, &attribution_base, &mut sequence)?;
            }
            Some(XmlEvent::EndElement(name)) => {
                require_name(&name, XSPF_NAMESPACE, "attribution")?;
                return Ok(());
            }
            Some(XmlEvent::Text(_)) => {
                return Err(XspfParseError::new(XspfParseErrorKind::TextNotAllowed));
            }
            None => return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedEof)),
        }
    }
}

/// Парсит один attribution URI child.
fn parse_attribution_child(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
    sequence: &mut ChildSequence,
) -> Result<(), XspfParseError> {
    require_namespace(element.name(), XSPF_NAMESPACE)?;
    let rank = match element.name().local_name() {
        "location" => 0,
        "identifier" => 1,
        _ => {
            return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedElement));
        }
    };
    sequence.accept(rank, ChildCardinality::Repeated)?;
    parse_uri_value(cursor, element, is_empty, parent_base).map(|_| ())
}

/// Валидирует XSPF link rel/content URI pair.
fn parse_link(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
) -> Result<(), XspfParseError> {
    validate_attributes(&element, &["rel"])?;
    let link_base = element_base(&element, parent_base)?;
    let relation = required_unqualified_attribute(&element, "rel")?;
    let _resolved_relation = resolve_location(&link_base, relation)?;
    let _resolved_content = parse_location_text(cursor, element.name(), is_empty, &link_base)?;
    Ok(())
}

/// Валидирует XSPF meta rel и plain-text content.
fn parse_meta(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
) -> Result<(), XspfParseError> {
    validate_attributes(&element, &["rel"])?;
    let meta_base = element_base(&element, parent_base)?;
    let relation = required_unqualified_attribute(&element, "rel")?;
    let _resolved_relation = resolve_location(&meta_base, relation)?;
    let _content = read_text_only(cursor, element.name(), is_empty)?;
    Ok(())
}

/// Common ignored plain-text child сохраняет schema checks.
fn parse_ignored_text(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
    sequence: &mut ChildSequence,
    rank: u8,
) -> Result<(), XspfParseError> {
    sequence.accept(rank, ChildCardinality::Optional)?;
    parse_text_value(cursor, element, is_empty, parent_base).map(|_| ())
}

/// Common ignored URI child всё равно проверяет encoding/base semantics.
fn parse_ignored_uri(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
    sequence: &mut ChildSequence,
    rank: u8,
) -> Result<(), XspfParseError> {
    sequence.accept(rank, ChildCardinality::Optional)?;
    parse_uri_value(cursor, element, is_empty, parent_base).map(|_| ())
}

/// Парсит XSPF/extension location с inherited element base.
pub(super) fn parse_location(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
) -> Result<XspfLocationCandidate, XspfParseError> {
    validate_attributes(&element, &[])?;
    let location_base = element_base(&element, parent_base)?;
    parse_location_text(cursor, element.name(), is_empty, &location_base)
}

/// Парсит URI-valued standard child.
fn parse_uri_value(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
) -> Result<XspfLocationCandidate, XspfParseError> {
    parse_location(cursor, element, is_empty, parent_base)
}

/// Парсит plain-text standard child и применяет его own xml:base scope.
fn parse_text_value(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
) -> Result<String, XspfParseError> {
    validate_attributes(&element, &[])?;
    let _text_base = element_base(&element, parent_base)?;
    read_text_only(cursor, element.name(), is_empty)
}

/// Читает location text и выполняет resolution.
fn parse_location_text(
    cursor: &mut EventCursor<'_>,
    expected_end: &XmlExpandedName,
    is_empty: bool,
    location_base: &Url,
) -> Result<XspfLocationCandidate, XspfParseError> {
    let raw_location = read_text_only(cursor, expected_end, is_empty)?;
    resolve_location(location_base, &raw_location)
}

/// Читает character-data-only content без nested markup.
fn read_text_only(
    cursor: &mut EventCursor<'_>,
    expected_end: &XmlExpandedName,
    is_empty: bool,
) -> Result<String, XspfParseError> {
    if is_empty {
        return Ok(String::new());
    }
    let mut content = String::new();
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::Text(text)) => content.push_str(text.content()),
            Some(XmlEvent::EndElement(name)) => {
                if &name != expected_end {
                    return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedElement));
                }
                return Ok(content);
            }
            Some(XmlEvent::StartElement(_)) | Some(XmlEvent::EmptyElement(_)) => {
                return Err(XspfParseError::new(XspfParseErrorKind::MarkupNotAllowed));
            }
            None => return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedEof)),
        }
    }
}

/// Iteratively skips unknown extension subtree без recursive allocation.
pub(super) fn skip_open_element(
    cursor: &mut EventCursor<'_>,
    expected_end: &XmlExpandedName,
    is_empty: bool,
) -> Result<(), XspfParseError> {
    if is_empty {
        return Ok(());
    }
    let mut nested_depth = 0usize;
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(_)) => {
                nested_depth = nested_depth
                    .checked_add(1)
                    .ok_or_else(|| XspfParseError::new(XspfParseErrorKind::UnexpectedElement))?;
            }
            Some(XmlEvent::EmptyElement(_)) | Some(XmlEvent::Text(_)) => {}
            Some(XmlEvent::EndElement(name)) if nested_depth == 0 => {
                if &name != expected_end {
                    return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedElement));
                }
                return Ok(());
            }
            Some(XmlEvent::EndElement(_)) => nested_depth -= 1,
            None => return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedEof)),
        }
    }
}

/// Применяет optional inherited `xml:base`.
pub(super) fn element_base(element: &XmlElement, parent_base: &Url) -> Result<Url, XspfParseError> {
    resolve_element_base(parent_base, xml_base_attribute(element))
}

/// Возвращает resolved `xml:base` attribute, если он присутствует.
fn xml_base_attribute(element: &XmlElement) -> Option<&str> {
    element
        .attributes()
        .iter()
        .find(|attribute| {
            attribute.name().namespace_uri() == Some(XML_NAMESPACE)
                && attribute.name().local_name() == "base"
        })
        .map(|attribute| attribute.value())
}

/// Разрешает unqualified required attribute.
pub(super) fn required_unqualified_attribute<'element>(
    element: &'element XmlElement,
    local_name: &str,
) -> Result<&'element str, XspfParseError> {
    element
        .attributes()
        .iter()
        .find(|attribute| {
            attribute.name().namespace_uri().is_none()
                && attribute.name().local_name() == local_name
        })
        .map(|attribute| attribute.value())
        .ok_or_else(|| XspfParseError::new(XspfParseErrorKind::MissingRequiredAttribute))
}

/// Запрещает attributes вне `xml:base` и exact element schema.
pub(super) fn validate_attributes(
    element: &XmlElement,
    allowed_unqualified: &[&str],
) -> Result<(), XspfParseError> {
    for attribute in element.attributes() {
        let is_xml_base = attribute.name().namespace_uri() == Some(XML_NAMESPACE)
            && attribute.name().local_name() == "base";
        let is_allowed_unqualified = attribute.name().namespace_uri().is_none()
            && allowed_unqualified.contains(&attribute.name().local_name());
        if !is_xml_base && !is_allowed_unqualified {
            return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedAttribute));
        }
    }
    Ok(())
}

/// Проверяет exact namespace/local expanded name.
pub(super) fn require_name(
    actual: &XmlExpandedName,
    namespace: &str,
    local_name: &str,
) -> Result<(), XspfParseError> {
    require_namespace(actual, namespace)?;
    if actual.local_name() != local_name {
        return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedElement));
    }
    Ok(())
}

/// Проверяет namespace отдельно для child dispatch.
fn require_namespace(actual: &XmlExpandedName, namespace: &str) -> Result<(), XspfParseError> {
    if actual.namespace_uri() != Some(namespace) {
        return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedNamespace));
    }
    Ok(())
}
