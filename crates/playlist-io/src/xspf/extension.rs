//! Versioned Rustiplayer playlist-level XSPF extension.

use bounded_xml_reader::{XmlElement, XmlEvent};
use url::Url;

use super::error::{XspfParseError, XspfParseErrorKind};
use super::limits::XspfParserLimits;
use super::model::{XspfGroup, XspfGroupTrackCount, XspfLocationCandidate, XspfTrackIndex};
use super::parser::{
    EventCursor, element_base, parse_location, require_name, required_unqualified_attribute,
    skip_open_element, validate_attributes,
};
use super::schema::parse_positive_u32;
use super::uri::validate_application_uri;
use super::{RUSTIPLAYER_XSPF_EXTENSION_NAMESPACE, XSPF_NAMESPACE};

/// Playlist-level extension исполняет только exact Rustiplayer application URI.
#[allow(clippy::too_many_arguments)]
pub(super) fn parse_playlist_extension(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
    limits: XspfParserLimits,
    groups: &mut Vec<XspfGroup>,
    rustiplayer_extension_seen: &mut bool,
) -> Result<(), XspfParseError> {
    validate_attributes(&element, &["application"])?;
    let extension_base = element_base(&element, parent_base)?;
    let application = required_unqualified_attribute(&element, "application")?;
    validate_application_uri(application)?;

    if application != RUSTIPLAYER_XSPF_EXTENSION_NAMESPACE {
        return skip_open_element(cursor, element.name(), is_empty);
    }
    if *rustiplayer_extension_seen {
        return Err(XspfParseError::new(
            XspfParseErrorKind::DuplicateRustiplayerExtension,
        ));
    }
    *rustiplayer_extension_seen = true;
    if is_empty {
        return Ok(());
    }

    loop {
        match cursor.next_container_event()? {
            Some(XmlEvent::StartElement(group_element)) => {
                push_group(
                    groups,
                    parse_group(cursor, group_element, false, &extension_base)?,
                    limits,
                )?;
            }
            Some(XmlEvent::EmptyElement(group_element)) => {
                push_group(
                    groups,
                    parse_group(cursor, group_element, true, &extension_base)?,
                    limits,
                )?;
            }
            Some(XmlEvent::EndElement(name)) => {
                require_name(&name, XSPF_NAMESPACE, "extension")?;
                return Ok(());
            }
            Some(XmlEvent::Text(_)) => {
                return Err(XspfParseError::new(XspfParseErrorKind::TextNotAllowed));
            }
            None => return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedEof)),
        }
    }
}

/// Track-level use known playlist extension rejected, unknown applications skipped.
pub(super) fn parse_track_extension(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
) -> Result<(), XspfParseError> {
    validate_attributes(&element, &["application"])?;
    let _extension_base = element_base(&element, parent_base)?;
    let application = required_unqualified_attribute(&element, "application")?;
    validate_application_uri(application)?;
    if application == RUSTIPLAYER_XSPF_EXTENSION_NAMESPACE {
        return Err(XspfParseError::new(
            XspfParseErrorKind::RustiplayerExtensionWrongScope,
        ));
    }
    skip_open_element(cursor, element.name(), is_empty)
}

/// Парсит одну minimal group record: range attributes + exactly one root location.
fn parse_group(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
) -> Result<XspfGroup, XspfParseError> {
    require_name(
        element.name(),
        RUSTIPLAYER_XSPF_EXTENSION_NAMESPACE,
        "group",
    )?;
    validate_attributes(&element, &["firstTrack", "trackCount"])?;
    let group_base = element_base(&element, parent_base)?;
    let first_track = parse_positive_u32(required_unqualified_attribute(&element, "firstTrack")?)?;
    let track_count = parse_positive_u32(required_unqualified_attribute(&element, "trackCount")?)?;
    let first_track = XspfTrackIndex::new(first_track)
        .ok_or_else(|| XspfParseError::new(XspfParseErrorKind::InvalidGroupRange))?;
    let track_count = XspfGroupTrackCount::new(track_count)
        .ok_or_else(|| XspfParseError::new(XspfParseErrorKind::InvalidGroupRange))?;

    if is_empty {
        return Err(XspfParseError::new(
            XspfParseErrorKind::MissingGroupLocation,
        ));
    }

    let root_location = match cursor.next_container_event()? {
        Some(XmlEvent::StartElement(location_element)) => {
            parse_extension_group_location(cursor, location_element, false, &group_base)?
        }
        Some(XmlEvent::EmptyElement(location_element)) => {
            parse_extension_group_location(cursor, location_element, true, &group_base)?
        }
        Some(XmlEvent::EndElement(_)) => {
            return Err(XspfParseError::new(
                XspfParseErrorKind::MissingGroupLocation,
            ));
        }
        Some(XmlEvent::Text(_)) => {
            return Err(XspfParseError::new(XspfParseErrorKind::TextNotAllowed));
        }
        None => return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedEof)),
    };

    match cursor.next_container_event()? {
        Some(XmlEvent::EndElement(name)) => {
            require_name(&name, RUSTIPLAYER_XSPF_EXTENSION_NAMESPACE, "group")?;
        }
        Some(_) => {
            return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedElement));
        }
        None => return Err(XspfParseError::new(XspfParseErrorKind::UnexpectedEof)),
    }

    Ok(XspfGroup::new(first_track, track_count, root_location))
}

/// Extension location использует extension namespace, чтобы не маскироваться под XSPF child.
fn parse_extension_group_location(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    is_empty: bool,
    parent_base: &Url,
) -> Result<XspfLocationCandidate, XspfParseError> {
    require_name(
        element.name(),
        RUSTIPLAYER_XSPF_EXTENSION_NAMESPACE,
        "location",
    )?;
    parse_location(cursor, element, is_empty, parent_base)
}

/// Проверяет group cap до Vec growth.
fn push_group(
    groups: &mut Vec<XspfGroup>,
    group: XspfGroup,
    limits: XspfParserLimits,
) -> Result<(), XspfParseError> {
    if groups.len() >= limits.maximum_groups() {
        return Err(XspfParseError::new(XspfParseErrorKind::GroupLimitExceeded));
    }
    groups.push(group);
    Ok(())
}

/// Финально проверяет sorted non-overlapping ranges против actual flattened tracks.
pub(super) fn validate_group_ranges(
    groups: &[XspfGroup],
    track_count: usize,
) -> Result<(), XspfParseError> {
    let mut previous_end = 0usize;
    for group in groups {
        let first_zero_based = usize::try_from(group.first_track().get() - 1)
            .map_err(|_| XspfParseError::new(XspfParseErrorKind::InvalidGroupRange))?;
        let group_count = usize::try_from(group.track_count().get())
            .map_err(|_| XspfParseError::new(XspfParseErrorKind::InvalidGroupRange))?;
        let exclusive_end = first_zero_based
            .checked_add(group_count)
            .ok_or_else(|| XspfParseError::new(XspfParseErrorKind::InvalidGroupRange))?;
        if first_zero_based < previous_end || exclusive_end > track_count {
            return Err(XspfParseError::new(XspfParseErrorKind::InvalidGroupRange));
        }
        previous_end = exclusive_end;
    }
    Ok(())
}
