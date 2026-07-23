//! Production composition selected yt-dlp HLS candidate -> uninstalled HLS VOD runtime.

use std::num::{NonZeroU8, NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use demux_api::{
    CompositeComponentLeadPolicy, DemuxInputCapability, DemuxRegistry, DemuxSniffBudget,
    ProgressiveDemuxBufferLimits,
};
use hls_playlist_core::HlsParserLimits;
use media_core::{DemuxRetryHint, Demuxer};
use rustiplayer_config::NetworkConfig;
use service_ytdlp::{
    YtDlpHlsManifestInputKind, YtDlpNormalizedCandidate, YtDlpTransportRequestContext,
};
use source_core::{CancellationToken, SourceRuntimeConfig};
use web_media_adaptive::{AdaptiveHttpContext, AdaptiveRetryPolicy, AdaptiveTransportLimits};
use web_media_core::{ContainerFamily, StreamLayout, TransportFamily};
use web_media_hls::{
    ExtractorAesOverride, HlsAudioLayoutIntent, HlsAudioRenditionEvidence,
    HlsComponentContainerIntent, HlsContainerEvidence, HlsMainTrackLayoutIntent, HlsManifestInput,
    HlsRequestOverrides, HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenPolicy,
    HlsVodOpenRequest, SecretInlineMediaPlaylist, prepare_hls_vod,
};
use web_media_transport_api::{SourceGeneration, TransportProviderId};

/// Secret-safe результат HLS preparation для общего coordinator-а.
pub(crate) struct PreparedHlsCandidate {
    pub(crate) demuxer: Box<dyn Demuxer + Send>,
    pub(crate) subtitles: Arc<[crate::web_media_hls_subtitles::InstalledHlsSubtitleRendition]>,
}

/// Typed граница между single-master alternate audio и extractor compound resources.
#[derive(Debug, thiserror::Error)]
enum HlsCandidateTopologyError {
    /// Два независимых manifest URL уже являются compound topology, а не AUDIO rendition master-а.
    #[error(
        "compound HLS candidate содержит два независимых manifest resource; alternate AUDIO поддерживается только внутри одного master playlist"
    )]
    IndependentManifestResources,
}

/// Проверяет transport family без открытия provider-а.
pub(crate) fn candidate_is_hls(candidate: &YtDlpNormalizedCandidate) -> bool {
    match candidate.descriptor().layout() {
        StreamLayout::Muxed(component) => component.transport().family() == TransportFamily::Hls,
        StreamLayout::VideoOnly(component) => {
            component.transport().family() == TransportFamily::Hls
        }
        StreamLayout::AudioOnly(component) => {
            component.transport().family() == TransportFamily::Hls
        }
        StreamLayout::Separate { video, audio } => {
            video.transport().family() == TransportFamily::Hls
                && audio.transport().family() == TransportFamily::Hls
        }
    }
}

/// Выполняет manifest/profile/container preflight на existing media-open worker-е.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_hls_candidate(
    candidate: &YtDlpNormalizedCandidate,
    provider_id: TransportProviderId,
    source_config: &SourceRuntimeConfig,
    network_config: &NetworkConfig,
    demux_registry: Arc<DemuxRegistry>,
    cancellation: CancellationToken,
) -> Result<PreparedHlsCandidate> {
    let generation = SourceGeneration::new(1);
    let context = YtDlpTransportRequestContext::new(provider_id, generation, cancellation.clone());
    let transport_request = candidate
        .hls_transport_request(&context)
        .context("Не удалось спроецировать yt-dlp HLS transport material")?;
    let material = candidate
        .hls_request_material()
        .context("Не удалось получить validated yt-dlp HLS material")?;
    let selected_target = transport_request.target().clone();
    let manifest = match material.manifest().kind() {
        YtDlpHlsManifestInputKind::FetchSelectedUrl => HlsManifestInput::Fetch {
            selected_url: selected_target,
        },
        YtDlpHlsManifestInputKind::Inline => HlsManifestInput::InlineMedia {
            selected_url: selected_target,
            playlist: SecretInlineMediaPlaylist::new(
                material
                    .manifest()
                    .inline_playlist_for_parse()
                    .ok_or_else(|| anyhow!("inline HLS type-state потерял playlist bytes"))?,
            ),
        },
    };
    let aes = material
        .aes_override()
        .map(|override_material| {
            ExtractorAesOverride::new(
                override_material.replacement_key_uri_for_fetch(),
                override_material.key_hex_for_crypto(),
                override_material.iv_hex_for_crypto(),
            )
        })
        .transpose()
        .context("yt-dlp hls_aes нарушил validated AES boundary")?;
    let (selection, containers) = selection_and_containers(candidate.descriptor().layout())?;
    let adaptive_limits = adaptive_limits(network_config)?;
    let http = AdaptiveHttpContext::new(
        transport_request,
        source_config,
        adaptive_limits,
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(3).expect("non-zero retry attempts"),
            Duration::from_millis(100),
            Duration::from_secs(2),
        )
        .context("HLS retry policy invalid")?,
    )
    .context("Не удалось создать HLS adaptive HTTP context")?;
    let policy = hls_policy(adaptive_limits)?;
    let opened = prepare_hls_vod(HlsVodOpenRequest {
        http,
        generation,
        manifest,
        selection,
        overrides: HlsRequestOverrides::new(aes),
        containers,
        demux_registry,
        policy,
    })
    .context("HLS VOD preflight завершился ошибкой")?;
    let subtitles = opened
        .subtitle_renditions()
        .iter()
        .map(crate::web_media_hls_subtitles::InstalledHlsSubtitleRendition::from_prepared)
        .collect::<Vec<_>>()
        .into();
    Ok(PreparedHlsCandidate {
        demuxer: opened.into_demuxer(),
        subtitles,
    })
}

fn selection_and_containers(
    layout: &StreamLayout,
) -> Result<(HlsVariantSelectionIntent, HlsComponentContainerIntent)> {
    let (video, audio_track, container, audio, main_track_layout) = match layout {
        StreamLayout::Muxed(component) => (
            Some(component.video()),
            Some(component.audio()),
            component.container(),
            HlsAudioLayoutIntent::ManifestResolved(audio_rendition_evidence(component.audio())),
            HlsMainTrackLayoutIntent::MuxedAv,
        ),
        StreamLayout::VideoOnly(component) => (
            Some(component.video()),
            None,
            component.container(),
            HlsAudioLayoutIntent::Muxed,
            HlsMainTrackLayoutIntent::VideoOnly,
        ),
        StreamLayout::AudioOnly(component) => (
            None,
            Some(component.audio()),
            component.container(),
            HlsAudioLayoutIntent::Muxed,
            HlsMainTrackLayoutIntent::AudioOnly,
        ),
        StreamLayout::Separate { .. } => {
            return Err(HlsCandidateTopologyError::IndependentManifestResources.into());
        }
    };
    let resolution = video
        .map(|track| -> Result<(NonZeroU32, NonZeroU32)> {
            let width = NonZeroU32::new(
                track
                    .width_pixels()
                    .ok_or_else(|| anyhow!("HLS video width evidence отсутствует"))?,
            )
            .ok_or_else(|| anyhow!("HLS video width равен нулю"))?;
            let height = NonZeroU32::new(
                track
                    .height()
                    .ok_or_else(|| anyhow!("HLS video height evidence отсутствует"))?
                    .pixels(),
            )
            .ok_or_else(|| anyhow!("HLS video height равен нулю"))?;
            Ok((width, height))
        })
        .transpose()?;
    let codecs = match (video, audio_track) {
        (Some(video), Some(audio)) => Some(
            format!(
                "{},{}",
                video.codec().raw().as_str(),
                audio.codec().raw().as_str()
            )
            .into_boxed_str(),
        ),
        (Some(video), None) => Some(video.codec().raw().as_str().into()),
        (None, Some(audio)) => Some(audio.codec().raw().as_str().into()),
        (None, None) => None,
    };
    Ok((
        HlsVariantSelectionIntent {
            resolution,
            codecs,
            audio,
            main_track_layout,
        },
        HlsComponentContainerIntent {
            main: HlsContainerEvidence::Exact(required_container(
                container
                    .consistent_family()
                    .map_err(|conflict| anyhow!("HLS container hints conflict: {conflict:?}"))?
                    .ok_or_else(|| anyhow!("HLS container evidence отсутствует"))?,
            )?),
            alternate_audio: matches!(layout, StreamLayout::Muxed(_))
                .then_some(HlsContainerEvidence::ContentProbe),
        },
    ))
}

/// Проецирует только extractor evidence, которое можно точно сопоставить с AUDIO rendition.
fn audio_rendition_evidence(
    audio: &web_media_core::AudioTrackDescriptor,
) -> HlsAudioRenditionEvidence {
    HlsAudioRenditionEvidence {
        name: None,
        language: audio.language().map(|language| language.as_str().into()),
        channel_count: audio
            .channels()
            .and_then(|channels| std::num::NonZeroU16::new(channels.get())),
    }
}

fn required_container(container: ContainerFamily) -> Result<HlsRequiredContainer> {
    match container {
        ContainerFamily::MpegTs => Ok(HlsRequiredContainer::TransportStream),
        ContainerFamily::IsoBmff | ContainerFamily::FragmentedIsoBmff => {
            Ok(HlsRequiredContainer::FragmentedMp4)
        }
        other => Err(anyhow!(
            "HLS container {other:?} не входит в TS/fMP4 profile"
        )),
    }
}

fn adaptive_limits(network: &NetworkConfig) -> Result<AdaptiveTransportLimits> {
    let maximum_segment_bytes = usize::try_from(network.memory_cache_mb)
        .ok()
        .and_then(|megabytes| megabytes.checked_mul(1_024 * 1_024))
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| anyhow!("network.memory_cache_mb нельзя выразить как HLS byte budget"))?;
    Ok(AdaptiveTransportLimits::new(
        NonZeroUsize::new(2 * 1_024 * 1_024).expect("manifest budget"),
        maximum_segment_bytes,
        NonZeroUsize::new(8_192).expect("VOD segment descriptor budget"),
    ))
}

fn hls_policy(limits: AdaptiveTransportLimits) -> Result<HlsVodOpenPolicy> {
    Ok(HlsVodOpenPolicy {
        parser_limits: HlsParserLimits::default(),
        demux_sniff_budget: DemuxSniffBudget::new(
            NonZeroUsize::new(64 * 1_024).expect("HLS sniff bytes"),
            NonZeroUsize::new(8).expect("HLS sniff segments"),
            Duration::from_secs(2),
        )?,
        progressive_limits: ProgressiveDemuxBufferLimits::new(
            NonZeroUsize::new(256).expect("HLS event queue"),
            NonZeroUsize::new(16 * 1_024 * 1_024).expect("HLS encoded queue"),
        ),
        retry_hint: DemuxRetryHint::new(Duration::from_millis(10))?,
        composite_lead_policy: CompositeComponentLeadPolicy::single_pending_packet(
            Duration::from_secs(3),
            NonZeroUsize::new(4 * 1_024 * 1_024).expect("HLS composite packet"),
        )?,
        maximum_key_resource_bytes: NonZeroUsize::new(64).expect("HLS key response"),
        maximum_seek_index_entries: NonZeroUsize::new(4_096).expect("HLS seek anchors"),
        maximum_seek_replay_events: NonZeroUsize::new(65_536).expect("HLS seek replay events"),
        maximum_seek_replay_bytes: limits.maximum_segment_bytes,
    })
}

/// Planner HLS transport output не делает TS playable для progressive HTTP rows.
pub(crate) fn hls_transport_input() -> DemuxInputCapability {
    DemuxInputCapability::OrderedSegments
}
