//! Pure bounded parser для authoritative `--dump-single-json` root line.

use std::collections::HashSet;
use std::time::Duration;

use serde_json::{Map, Value};

use super::limits::{
    TOPOLOGY_IDENTITY_MAX_UTF8_BYTES, TOPOLOGY_LOCATOR_MAX_UTF8_BYTES,
    TOPOLOGY_SUMMARY_TEXT_MAX_UTF8_BYTES, YtDlpTopologyBudgets, YtDlpTopologyError,
    YtDlpTopologyInvalidResponseReason,
};
use super::model::{
    YtDlpDelegationSummaryPolicy, YtDlpTopology, YtDlpTopologyCollection, YtDlpTopologyDelegation,
    YtDlpTopologyEntry, YtDlpTopologyIdentity, YtDlpTopologyMultiVideo, YtDlpTopologySummary,
    YtDlpTopologySummaryFieldValue, YtDlpTopologySummaryUnavailableReason, YtDlpTopologyVideo,
    YtDlpUnavailableTopologyEntry, YtDlpUnavailableTopologyReason,
};
use crate::parse_yt_dlp_media_locator;

/// Парсит последнюю authoritative JSON line и не доверяет `n_entries`.
pub(crate) fn parse_topology_root(
    root_json_line: &[u8],
    budgets: YtDlpTopologyBudgets,
) -> Result<YtDlpTopology, YtDlpTopologyError> {
    validate_json_depth(root_json_line, budgets.json_depth)?;
    let root_value: Value = serde_json::from_slice(root_json_line).map_err(|_| {
        YtDlpTopologyError::invalid(YtDlpTopologyInvalidResponseReason::MalformedJson)
    })?;
    let mut parser = TopologyParser::new(budgets);
    parser.parse_root(&root_value)
}

/// Проверяет каждую preceding lazy line как standalone JSON object.
pub(crate) fn validate_lazy_json_lines(
    lazy_json_lines: &[Vec<u8>],
    budgets: YtDlpTopologyBudgets,
) -> Result<(), YtDlpTopologyError> {
    for json_line in lazy_json_lines {
        validate_json_depth(json_line, budgets.json_depth)?;
        let value: Value = serde_json::from_slice(json_line).map_err(|_| {
            YtDlpTopologyError::invalid(YtDlpTopologyInvalidResponseReason::MalformedJson)
        })?;
        if !value.is_object() {
            return Err(YtDlpTopologyError::invalid(
                YtDlpTopologyInvalidResponseReason::ExpectedObject,
            ));
        }
    }

    Ok(())
}

struct TopologyParser {
    budgets: YtDlpTopologyBudgets,
    retained_entries: usize,
    active_identities: HashSet<String>,
}

impl TopologyParser {
    fn new(budgets: YtDlpTopologyBudgets) -> Self {
        Self {
            budgets,
            retained_entries: 0,
            active_identities: HashSet::new(),
        }
    }

    fn parse_root(&mut self, value: &Value) -> Result<YtDlpTopology, YtDlpTopologyError> {
        let object = object(value)?;
        match result_type(object)? {
            ResultType::Video => self.parse_video(object).map(YtDlpTopology::Video),
            ResultType::Playlist => self
                .with_active_identity(object, 1, |parser| parser.parse_collection(object, 1))
                .map(YtDlpTopology::Playlist),
            ResultType::MultiVideo => self
                .with_active_identity(object, 1, |parser| parser.parse_multi_video(object, 1))
                .map(YtDlpTopology::MultiVideo),
            ResultType::Url => self
                .parse_delegation(object, YtDlpDelegationSummaryPolicy::ResolvedResultOnly)
                .map(YtDlpTopology::Delegation),
            ResultType::UrlTransparent => self
                .parse_delegation(
                    object,
                    YtDlpDelegationSummaryPolicy::TransparentWrapperPriority,
                )
                .map(YtDlpTopology::Delegation),
        }
    }

    fn parse_entry(
        &mut self,
        value: &Value,
        depth: usize,
    ) -> Result<YtDlpTopologyEntry, YtDlpTopologyError> {
        self.retained_entries = self.retained_entries.saturating_add(1);
        if self.retained_entries > self.budgets.entry_count {
            return Err(YtDlpTopologyError::EntryBudgetExceeded);
        }
        if depth > self.budgets.topology_depth {
            return Err(YtDlpTopologyError::TopologyDepthExceeded);
        }
        if value.is_null() {
            return Ok(YtDlpTopologyEntry::Unavailable(
                YtDlpUnavailableTopologyEntry::new(
                    missing_identity(),
                    empty_summary(),
                    YtDlpUnavailableTopologyReason::NullEntry,
                ),
            ));
        }

        let object = object(value)?;
        if has_restricted_availability(object) {
            return Ok(YtDlpTopologyEntry::Unavailable(
                YtDlpUnavailableTopologyEntry::new(
                    parse_identity(object)?,
                    parse_summary(object),
                    YtDlpUnavailableTopologyReason::RestrictedAvailability,
                ),
            ));
        }

        match result_type(object)? {
            ResultType::Video => {
                let video = self.parse_video(object)?;
                if video.identity().is_missing() {
                    return Ok(YtDlpTopologyEntry::Unavailable(
                        YtDlpUnavailableTopologyEntry::new(
                            parse_identity(object)?,
                            parse_summary(object),
                            YtDlpUnavailableTopologyReason::MissingIdentity,
                        ),
                    ));
                }
                Ok(YtDlpTopologyEntry::Video(video))
            }
            ResultType::Playlist => self
                .with_active_identity(object, depth, |parser| {
                    parser.parse_collection(object, depth)
                })
                .map(YtDlpTopologyEntry::Playlist),
            ResultType::MultiVideo => self
                .with_active_identity(object, depth, |parser| {
                    parser.parse_multi_video(object, depth)
                })
                .map(YtDlpTopologyEntry::MultiVideo),
            ResultType::Url => self
                .parse_delegation_entry(object, YtDlpDelegationSummaryPolicy::ResolvedResultOnly),
            ResultType::UrlTransparent => self.parse_delegation_entry(
                object,
                YtDlpDelegationSummaryPolicy::TransparentWrapperPriority,
            ),
        }
    }

    fn parse_video(
        &self,
        object: &Map<String, Value>,
    ) -> Result<YtDlpTopologyVideo, YtDlpTopologyError> {
        let identity = parse_identity(object)?;
        let summary = parse_summary(object);
        if identity.extractor_id().is_none_or(str::is_empty) {
            return Err(YtDlpTopologyError::invalid(
                YtDlpTopologyInvalidResponseReason::MissingRequiredField,
            ));
        }
        if !has_video_source_description(object) {
            return Err(YtDlpTopologyError::invalid(
                YtDlpTopologyInvalidResponseReason::MissingVideoSourceDescription,
            ));
        }

        Ok(YtDlpTopologyVideo::new(identity, summary))
    }

    fn parse_collection(
        &mut self,
        object: &Map<String, Value>,
        depth: usize,
    ) -> Result<YtDlpTopologyCollection, YtDlpTopologyError> {
        let entries = self.parse_entries(object, depth)?;
        Ok(YtDlpTopologyCollection::new(
            parse_identity(object)?,
            parse_summary(object),
            entries,
        ))
    }

    fn parse_multi_video(
        &mut self,
        object: &Map<String, Value>,
        depth: usize,
    ) -> Result<YtDlpTopologyMultiVideo, YtDlpTopologyError> {
        let root_video = self.parse_video(object)?;
        let entries = self.parse_entries(object, depth)?;
        Ok(YtDlpTopologyMultiVideo::new(root_video, entries))
    }

    fn parse_entries(
        &mut self,
        object: &Map<String, Value>,
        depth: usize,
    ) -> Result<Vec<YtDlpTopologyEntry>, YtDlpTopologyError> {
        let entries = object
            .get("entries")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                YtDlpTopologyError::invalid(YtDlpTopologyInvalidResponseReason::MissingEntries)
            })?;
        let child_depth = depth.saturating_add(1);
        if child_depth > self.budgets.topology_depth && !entries.is_empty() {
            return Err(YtDlpTopologyError::TopologyDepthExceeded);
        }

        entries
            .iter()
            .map(|entry| self.parse_entry(entry, child_depth))
            .collect()
    }

    fn parse_delegation_entry(
        &self,
        object: &Map<String, Value>,
        merge_policy: YtDlpDelegationSummaryPolicy,
    ) -> Result<YtDlpTopologyEntry, YtDlpTopologyError> {
        match object.get("url").and_then(Value::as_str) {
            Some(_) => self
                .parse_delegation(object, merge_policy)
                .map(YtDlpTopologyEntry::Delegation),
            None => {
                let identity = parse_identity(object)?;
                let reason = if identity.is_missing() {
                    YtDlpUnavailableTopologyReason::MissingIdentity
                } else {
                    YtDlpUnavailableTopologyReason::MissingDelegationTarget
                };
                Ok(YtDlpTopologyEntry::Unavailable(
                    YtDlpUnavailableTopologyEntry::new(identity, parse_summary(object), reason),
                ))
            }
        }
    }

    fn parse_delegation(
        &self,
        object: &Map<String, Value>,
        merge_policy: YtDlpDelegationSummaryPolicy,
    ) -> Result<YtDlpTopologyDelegation, YtDlpTopologyError> {
        let target_text = required_string(object, "url")?;
        if target_text.len() > TOPOLOGY_LOCATOR_MAX_UTF8_BYTES {
            return Err(YtDlpTopologyError::invalid(
                YtDlpTopologyInvalidResponseReason::FieldBudgetExceeded,
            ));
        }
        let target = parse_yt_dlp_media_locator(target_text)
            .map_err(|source| YtDlpTopologyError::DelegationLocator { source })?;

        Ok(YtDlpTopologyDelegation::new(
            target,
            parse_summary(object),
            merge_policy,
        ))
    }

    fn with_active_identity<T>(
        &mut self,
        object: &Map<String, Value>,
        depth: usize,
        operation: impl FnOnce(&mut Self) -> Result<T, YtDlpTopologyError>,
    ) -> Result<T, YtDlpTopologyError> {
        if depth > self.budgets.topology_depth {
            return Err(YtDlpTopologyError::TopologyDepthExceeded);
        }
        let active_key = active_identity_key(object)?;
        if let Some(identity_key) = active_key.as_ref()
            && !self.active_identities.insert(identity_key.clone())
        {
            return Err(YtDlpTopologyError::invalid(
                YtDlpTopologyInvalidResponseReason::DelegationCycle,
            ));
        }

        let result = operation(self);
        if let Some(identity_key) = active_key {
            self.active_identities.remove(&identity_key);
        }
        result
    }
}

#[derive(Clone, Copy)]
enum ResultType {
    Video,
    Playlist,
    MultiVideo,
    Url,
    UrlTransparent,
}

fn result_type(object: &Map<String, Value>) -> Result<ResultType, YtDlpTopologyError> {
    match object.get("_type") {
        None | Some(Value::Null) => Ok(ResultType::Video),
        Some(Value::String(result_type)) if result_type == "video" => Ok(ResultType::Video),
        Some(Value::String(result_type)) if result_type == "playlist" => Ok(ResultType::Playlist),
        Some(Value::String(result_type)) if result_type == "multi_video" => {
            Ok(ResultType::MultiVideo)
        }
        Some(Value::String(result_type)) if result_type == "url" => Ok(ResultType::Url),
        Some(Value::String(result_type)) if result_type == "url_transparent" => {
            Ok(ResultType::UrlTransparent)
        }
        _ => Err(YtDlpTopologyError::invalid(
            YtDlpTopologyInvalidResponseReason::UnsupportedResultType,
        )),
    }
}

fn parse_identity(
    object: &Map<String, Value>,
) -> Result<YtDlpTopologyIdentity, YtDlpTopologyError> {
    let extractor_id = optional_bounded_string(object, "id", TOPOLOGY_IDENTITY_MAX_UTF8_BYTES)?;
    let extractor_key =
        optional_bounded_string(object, "extractor_key", TOPOLOGY_IDENTITY_MAX_UTF8_BYTES)?.or(
            optional_bounded_string(object, "ie_key", TOPOLOGY_IDENTITY_MAX_UTF8_BYTES)?,
        );
    let webpage_locator = optional_locator(object, "webpage_url")?;
    let original_locator = optional_locator(object, "original_url")?;

    Ok(YtDlpTopologyIdentity::new(
        extractor_id,
        extractor_key,
        webpage_locator,
        original_locator,
    ))
}

fn parse_summary(object: &Map<String, Value>) -> YtDlpTopologySummary {
    // Topology переносит только компактный label; rich description принадлежит будущему details API.
    let title = parse_optional_summary_text(object, "title", TOPOLOGY_SUMMARY_TEXT_MAX_UTF8_BYTES);
    // Duration остаётся hint-ом и не имеет права ломать structurally playable node.
    let duration = parse_optional_summary_duration(object);

    YtDlpTopologySummary::new(title, duration)
}

/// Парсит optional compact text без молчаливой обрезки и без fatal topology failure.
fn parse_optional_summary_text(
    object: &Map<String, Value>,
    field: &str,
    max_utf8_bytes: usize,
) -> YtDlpTopologySummaryFieldValue<String> {
    let Some(value) = object.get(field) else {
        return YtDlpTopologySummaryFieldValue::Missing;
    };
    if value.is_null() {
        return YtDlpTopologySummaryFieldValue::Missing;
    }
    let Some(text) = value.as_str() else {
        return YtDlpTopologySummaryFieldValue::Unavailable(
            YtDlpTopologySummaryUnavailableReason::UnexpectedType,
        );
    };
    if text.is_empty() {
        return YtDlpTopologySummaryFieldValue::Unavailable(
            YtDlpTopologySummaryUnavailableReason::EmptyValue,
        );
    }
    if text.len() > max_utf8_bytes {
        return YtDlpTopologySummaryFieldValue::Unavailable(
            YtDlpTopologySummaryUnavailableReason::FieldBudgetExceeded,
        );
    }

    YtDlpTopologySummaryFieldValue::Available(text.to_owned())
}

/// Парсит optional duration hint, сохраняя malformed distinction внутри summary.
fn parse_optional_summary_duration(
    object: &Map<String, Value>,
) -> YtDlpTopologySummaryFieldValue<Duration> {
    let Some(value) = object.get("duration") else {
        return YtDlpTopologySummaryFieldValue::Missing;
    };
    if value.is_null() {
        return YtDlpTopologySummaryFieldValue::Missing;
    }
    let Some(seconds) = value.as_f64() else {
        return YtDlpTopologySummaryFieldValue::Unavailable(
            YtDlpTopologySummaryUnavailableReason::UnexpectedType,
        );
    };
    if !seconds.is_finite() || seconds.is_sign_negative() {
        return YtDlpTopologySummaryFieldValue::Unavailable(
            YtDlpTopologySummaryUnavailableReason::InvalidNumericValue,
        );
    }
    let Ok(duration) = Duration::try_from_secs_f64(seconds) else {
        return YtDlpTopologySummaryFieldValue::Unavailable(
            YtDlpTopologySummaryUnavailableReason::InvalidNumericValue,
        );
    };

    YtDlpTopologySummaryFieldValue::Available(duration)
}

fn optional_bounded_string(
    object: &Map<String, Value>,
    field: &str,
    max_utf8_bytes: usize,
) -> Result<Option<String>, YtDlpTopologyError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let text = value.as_str().ok_or_else(|| {
        YtDlpTopologyError::invalid(YtDlpTopologyInvalidResponseReason::MissingRequiredField)
    })?;
    if text.len() > max_utf8_bytes {
        return Err(YtDlpTopologyError::invalid(
            YtDlpTopologyInvalidResponseReason::FieldBudgetExceeded,
        ));
    }

    Ok(Some(text.to_owned()))
}

fn optional_locator(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<crate::YtDlpMediaLocator>, YtDlpTopologyError> {
    let Some(locator_text) = object.get(field).and_then(Value::as_str) else {
        return Ok(None);
    };
    if locator_text.len() > TOPOLOGY_LOCATOR_MAX_UTF8_BYTES {
        return Err(YtDlpTopologyError::invalid(
            YtDlpTopologyInvalidResponseReason::FieldBudgetExceeded,
        ));
    }
    let locator = parse_yt_dlp_media_locator(locator_text)
        .map_err(|source| YtDlpTopologyError::DelegationLocator { source })?;
    Ok(Some(locator))
}

fn required_string<'value>(
    object: &'value Map<String, Value>,
    field: &str,
) -> Result<&'value str, YtDlpTopologyError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            YtDlpTopologyError::invalid(YtDlpTopologyInvalidResponseReason::MissingRequiredField)
        })
}

fn object(value: &Value) -> Result<&Map<String, Value>, YtDlpTopologyError> {
    value.as_object().ok_or_else(|| {
        YtDlpTopologyError::invalid(YtDlpTopologyInvalidResponseReason::ExpectedObject)
    })
}

fn has_video_source_description(object: &Map<String, Value>) -> bool {
    let has_direct_url = object
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(|url| !url.is_empty());
    let has_formats = object
        .get("formats")
        .and_then(Value::as_array)
        .is_some_and(|formats| !formats.is_empty());
    has_direct_url || has_formats
}

fn has_restricted_availability(object: &Map<String, Value>) -> bool {
    object
        .get("availability")
        .and_then(Value::as_str)
        .is_some_and(|availability| !matches!(availability, "public" | "unlisted"))
}

fn active_identity_key(object: &Map<String, Value>) -> Result<Option<String>, YtDlpTopologyError> {
    if let Some(webpage_url) = object.get("webpage_url").and_then(Value::as_str) {
        if webpage_url.len() > TOPOLOGY_LOCATOR_MAX_UTF8_BYTES {
            return Err(YtDlpTopologyError::invalid(
                YtDlpTopologyInvalidResponseReason::FieldBudgetExceeded,
            ));
        }
        return Ok(Some(format!("url:{webpage_url}")));
    }
    if let Some(extractor_id) = object.get("id").and_then(Value::as_str) {
        if extractor_id.len() > TOPOLOGY_IDENTITY_MAX_UTF8_BYTES {
            return Err(YtDlpTopologyError::invalid(
                YtDlpTopologyInvalidResponseReason::FieldBudgetExceeded,
            ));
        }
        let extractor_key = object
            .get("extractor_key")
            .or_else(|| object.get("ie_key"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Ok(Some(format!("id:{extractor_key}:{extractor_id}")));
    }

    Ok(None)
}

fn missing_identity() -> YtDlpTopologyIdentity {
    YtDlpTopologyIdentity::new(None, None, None, None)
}

fn empty_summary() -> YtDlpTopologySummary {
    YtDlpTopologySummary::new(
        YtDlpTopologySummaryFieldValue::Missing,
        YtDlpTopologySummaryFieldValue::Missing,
    )
}

pub(super) fn validate_json_depth(
    json_bytes: &[u8],
    max_depth: usize,
) -> Result<(), YtDlpTopologyError> {
    let mut current_depth = 0usize;
    let mut inside_string = false;
    let mut escaped = false;
    for byte in json_bytes {
        if inside_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                inside_string = false;
            }
            continue;
        }

        match *byte {
            b'"' => inside_string = true,
            b'{' | b'[' => {
                current_depth = current_depth.saturating_add(1);
                if current_depth > max_depth {
                    return Err(YtDlpTopologyError::JsonDepthExceeded);
                }
            }
            b'}' | b']' => current_depth = current_depth.saturating_sub(1),
            _ => {}
        }
    }

    Ok(())
}
