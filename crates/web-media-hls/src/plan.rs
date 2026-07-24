use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use demux_api::{OrderedSegmentDiscontinuity, OrderedSegmentKind};
use hls_playlist_core::{ByteRange, HlsDuration, InitializationMap, MediaPlaylist, MediaSegment};
use source_core::{HttpBoundedByteRange, HttpRequestTarget};

use crate::{
    ActiveAes128Key, Aes128InitializationVector, Aes128KeySource, HlsKeyState, HlsKeyStateError,
    HlsRequestOverrides, HlsRequiredContainer, SecretAes128Key,
};

/// Secret key material либо exact target для lazy current-key fetch.
#[derive(Clone, Debug)]
pub(crate) enum PlannedKeySource {
    Inline(SecretAes128Key),
    /// Manifest key URI получает scoped key-query merge на каждом authorised hop.
    ManifestTarget(HttpRequestTarget),
    /// `hls_aes.uri` является exact replacement и намеренно обходит key-query merge.
    ExtractorReplacement(HttpRequestTarget),
}

/// Per-resource AES state; CBC chain всегда начинается заново.
#[derive(Clone, Debug)]
pub(crate) struct PlannedEncryption {
    pub key_identity: u64,
    pub key: PlannedKeySource,
    pub iv: Aes128InitializationVector,
}

/// Один exact MAP/media resource после URI/query/range/AES validation.
#[derive(Clone, Debug)]
pub(crate) struct PlannedResource {
    pub kind: OrderedSegmentKind,
    pub discontinuity: OrderedSegmentDiscontinuity,
    pub target: HttpRequestTarget,
    pub byte_range: Option<HttpBoundedByteRange>,
    pub encryption: Option<PlannedEncryption>,
}

/// Один parser/decoder-facing lifecycle epoch.
#[derive(Clone, Debug)]
pub(crate) struct HlsEpochPlan {
    pub resources: Vec<PlannedResource>,
    pub timeline_start: Duration,
}

/// Полный finite component plan без fetched media bytes.
#[derive(Clone, Debug)]
pub(crate) struct HlsComponentPlan {
    pub container: HlsRequiredContainer,
    pub epochs: Vec<HlsEpochPlan>,
    pub duration: Duration,
}

impl HlsComponentPlan {
    /// Проверяет statically known exact ranges до publication deferred runtime-а.
    pub(crate) fn validate_resource_bound(
        &self,
        maximum_resource_bytes: NonZeroUsize,
    ) -> Result<(), HlsPlanError> {
        if self
            .epochs
            .iter()
            .flat_map(|epoch| &epoch.resources)
            .filter_map(|resource| resource.byte_range)
            .any(|range| range.length() > maximum_resource_bytes)
        {
            return Err(HlsPlanError::ResourceRangeExceedsAdaptiveLimit);
        }
        Ok(())
    }
}

/// Secret-safe статическая ошибка до publication segment/demux runtime-а.
#[derive(Debug, thiserror::Error)]
pub enum HlsPlanError {
    #[error("HLS fMP4 media playlist содержит segment без required EXT-X-MAP")]
    FragmentedMp4MapRequired,
    #[error("HLS media playlist не содержит сегментов")]
    EmptyMediaPlaylist,
    #[error("HLS byte range имеет нулевую либо непредставимую длину")]
    InvalidByteRangeLength,
    #[error("implicit HLS byte range не продолжает предыдущий range того же resource")]
    MissingImplicitByteRangeBase,
    #[error("HLS byte range offset переполняется")]
    ByteRangeOverflow,
    #[error("HLS duration не помещается в runtime timeline")]
    DurationOverflow,
    #[error("HLS duration имеет invalid validated representation")]
    InvalidDuration,
    #[error("HLS resource target invalid: {0}")]
    Target(#[from] source_core::HttpRequestTargetError),
    #[error("HLS exact HTTP range invalid")]
    HttpRange,
    #[error("HLS exact byte range превышает shared adaptive resource limit")]
    ResourceRangeExceedsAdaptiveLimit,
    #[error("HLS AES profile invalid: {0}")]
    Key(#[from] HlsKeyStateError),
    #[error("HLS encrypted resource потерял declaration identity")]
    MissingKeyIdentity,
}

/// Строит immutable lazy-fetch plan и проверяет все статически известные инварианты.
pub(crate) fn build_component_plan(
    media: &MediaPlaylist,
    container: HlsRequiredContainer,
    base: &HttpRequestTarget,
    overrides: &HlsRequestOverrides,
) -> Result<HlsComponentPlan, HlsPlanError> {
    build_component_plan_with_epoch_strategy(media, container, base, overrides, false)
}

/// Строит live-only plan, где каждый media segment является отдельным epoch.
///
/// Такая стратегия дороже finite VOD plan-а, но сохраняет точную связь
/// segment identity с packet/RAP без расширения generic demux API.
pub(crate) fn build_segment_scoped_component_plan(
    media: &MediaPlaylist,
    container: HlsRequiredContainer,
    base: &HttpRequestTarget,
    overrides: &HlsRequestOverrides,
) -> Result<HlsComponentPlan, HlsPlanError> {
    build_component_plan_with_epoch_strategy(media, container, base, overrides, true)
}

fn build_component_plan_with_epoch_strategy(
    media: &MediaPlaylist,
    container: HlsRequiredContainer,
    base: &HttpRequestTarget,
    overrides: &HlsRequestOverrides,
    segment_scoped_epochs: bool,
) -> Result<HlsComponentPlan, HlsPlanError> {
    if media.segments.is_empty() {
        return Err(HlsPlanError::EmptyMediaPlaylist);
    }
    if container == HlsRequiredContainer::FragmentedMp4
        && media
            .segments
            .iter()
            .any(|segment| segment.initialization_map.is_none())
    {
        return Err(HlsPlanError::FragmentedMp4MapRequired);
    }

    let mut range_cursor = RangeCursor::default();
    let mut epochs = Vec::new();
    let mut current_resources = Vec::new();
    let mut current_map: Option<InitializationMap> = None;
    let mut current_map_resource: Option<PlannedResource> = None;
    let mut timeline_start = Duration::ZERO;
    let mut current_epoch_duration = Duration::ZERO;

    for segment in &media.segments {
        let map_changed = segment.initialization_map != current_map;
        let starts_epoch = !current_resources.is_empty()
            && (segment_scoped_epochs || segment.discontinuity || map_changed);
        if starts_epoch {
            epochs.push(HlsEpochPlan {
                resources: std::mem::take(&mut current_resources),
                timeline_start,
            });
            timeline_start = timeline_start
                .checked_add(current_epoch_duration)
                .ok_or(HlsPlanError::DurationOverflow)?;
            current_epoch_duration = Duration::ZERO;
        }

        if let Some(initialization_map) = segment
            .initialization_map
            .as_ref()
            .filter(|_| current_resources.is_empty() || map_changed)
        {
            let map_resource = if current_map.as_ref() == Some(initialization_map) {
                current_map_resource
                    .clone()
                    .ok_or(HlsPlanError::FragmentedMp4MapRequired)?
            } else {
                plan_initialization_map(initialization_map, base, overrides, &mut range_cursor)?
            };
            current_map = Some(initialization_map.clone());
            current_map_resource = Some(map_resource.clone());
            current_resources.push(map_resource);
        } else if map_changed {
            current_map = None;
            current_map_resource = None;
        }

        current_resources.push(plan_media_segment(
            segment,
            container,
            base,
            overrides,
            &mut range_cursor,
        )?);
        current_epoch_duration = current_epoch_duration
            .checked_add(parse_hls_duration(&segment.duration)?)
            .ok_or(HlsPlanError::DurationOverflow)?;
    }
    if !current_resources.is_empty() {
        epochs.push(HlsEpochPlan {
            resources: current_resources,
            timeline_start,
        });
    }
    let duration = media
        .segments
        .iter()
        .try_fold(Duration::ZERO, |total, segment| {
            total
                .checked_add(parse_hls_duration(&segment.duration)?)
                .ok_or(HlsPlanError::DurationOverflow)
        })?;
    Ok(HlsComponentPlan {
        container,
        epochs,
        duration,
    })
}

fn plan_initialization_map(
    initialization_map: &InitializationMap,
    base: &HttpRequestTarget,
    overrides: &HlsRequestOverrides,
    range_cursor: &mut RangeCursor,
) -> Result<PlannedResource, HlsPlanError> {
    let target = resource_target(base, initialization_map.uri.expose_for_resolution())?;
    let byte_range = range_cursor.resolve(&target, initialization_map.byte_range.as_ref())?;
    let encryption =
        HlsKeyState::active_for_initialization_map(initialization_map, overrides.aes())?
            .map(|active| {
                let key_identity = initialization_map
                    .key
                    .as_ref()
                    .map(hls_playlist_core::HlsKeyDeclaration::declaration_sequence)
                    .ok_or(HlsPlanError::MissingKeyIdentity)?;
                planned_encryption(active, key_identity, base, None)
            })
            .transpose()?;
    Ok(PlannedResource {
        kind: OrderedSegmentKind::Initialization,
        discontinuity: OrderedSegmentDiscontinuity::Continuous,
        target,
        byte_range,
        encryption,
    })
}

fn plan_media_segment(
    segment: &MediaSegment,
    container: HlsRequiredContainer,
    base: &HttpRequestTarget,
    overrides: &HlsRequestOverrides,
    range_cursor: &mut RangeCursor,
) -> Result<PlannedResource, HlsPlanError> {
    let target = resource_target(base, segment.uri.expose_for_resolution())?;
    let byte_range = range_cursor.resolve(&target, segment.byte_range.as_ref())?;
    let encryption = match segment.key.as_ref() {
        Some(declaration) => {
            let mut state = HlsKeyState::default();
            state.apply(declaration, overrides.aes())?;
            state
                .active()
                .cloned()
                .map(|active| {
                    let iv = active.iv_for_media_segment(segment.media_sequence);
                    planned_encryption(active, declaration.declaration_sequence(), base, Some(iv))
                })
                .transpose()?
        }
        None => None,
    };
    let discontinuity =
        if container == HlsRequiredContainer::TransportStream && segment.discontinuity {
            OrderedSegmentDiscontinuity::StartsNewTimeline
        } else {
            OrderedSegmentDiscontinuity::Continuous
        };
    Ok(PlannedResource {
        kind: OrderedSegmentKind::Media,
        discontinuity,
        target,
        byte_range,
        encryption,
    })
}

fn planned_encryption(
    active: ActiveAes128Key,
    key_identity: u64,
    base: &HttpRequestTarget,
    media_iv: Option<Aes128InitializationVector>,
) -> Result<PlannedEncryption, HlsPlanError> {
    let iv = match media_iv {
        Some(iv) => iv,
        None => active.iv_for_initialization_map()?,
    };
    let key = match active.source() {
        Aes128KeySource::Inline(key) => PlannedKeySource::Inline(key.clone()),
        Aes128KeySource::ManifestReference(reference) => PlannedKeySource::ManifestTarget(
            resource_target(base, reference.expose_for_resolution())?,
        ),
        Aes128KeySource::ExtractorReplacement(reference) => PlannedKeySource::ExtractorReplacement(
            base.resolve_reference(reference.expose_for_resolution())?,
        ),
    };
    Ok(PlannedEncryption {
        key_identity,
        key,
        iv,
    })
}

fn resource_target(
    base: &HttpRequestTarget,
    reference: &str,
) -> Result<HttpRequestTarget, source_core::HttpRequestTargetError> {
    base.resolve_reference(reference)
}

#[derive(Default)]
struct RangeCursor {
    previous: Option<(HttpRequestTarget, u64)>,
}

impl RangeCursor {
    fn resolve(
        &mut self,
        target: &HttpRequestTarget,
        byte_range: Option<&ByteRange>,
    ) -> Result<Option<HttpBoundedByteRange>, HlsPlanError> {
        let Some(byte_range) = byte_range else {
            self.previous = None;
            return Ok(None);
        };
        let length =
            NonZeroU64::new(byte_range.length).ok_or(HlsPlanError::InvalidByteRangeLength)?;
        let length_usize = usize::try_from(length.get())
            .ok()
            .and_then(NonZeroUsize::new)
            .ok_or(HlsPlanError::InvalidByteRangeLength)?;
        let start = match byte_range.offset {
            Some(offset) => offset,
            None => self
                .previous
                .as_ref()
                .filter(|(previous_target, _)| previous_target == target)
                .map(|(_, end)| *end)
                .ok_or(HlsPlanError::MissingImplicitByteRangeBase)?,
        };
        let end = start
            .checked_add(length.get())
            .ok_or(HlsPlanError::ByteRangeOverflow)?;
        let range =
            HttpBoundedByteRange::new(start, length_usize).map_err(|_| HlsPlanError::HttpRange)?;
        self.previous = Some((target.clone(), end));
        Ok(Some(range))
    }
}

pub(crate) fn parse_hls_duration(duration: &HlsDuration) -> Result<Duration, HlsPlanError> {
    let text = duration.as_decimal_str();
    let (seconds_text, fractional_text) = text.split_once('.').unwrap_or((text, ""));
    let mut seconds = seconds_text
        .parse::<u64>()
        .map_err(|_| HlsPlanError::InvalidDuration)?;
    if !fractional_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(HlsPlanError::InvalidDuration);
    }
    let mut nanoseconds = 0u32;
    for (index, byte) in fractional_text.bytes().take(9).enumerate() {
        let digit = byte
            .checked_sub(b'0')
            .filter(|digit| *digit <= 9)
            .ok_or(HlsPlanError::InvalidDuration)?;
        nanoseconds += u32::from(digit) * 10u32.pow(8 - index as u32);
    }
    if fractional_text.len() > 9 && fractional_text.as_bytes()[9] >= b'5' {
        if nanoseconds == 999_999_999 {
            seconds = seconds
                .checked_add(1)
                .ok_or(HlsPlanError::DurationOverflow)?;
            nanoseconds = 0;
        } else {
            nanoseconds += 1;
        }
    }
    Ok(Duration::new(seconds, nanoseconds))
}
