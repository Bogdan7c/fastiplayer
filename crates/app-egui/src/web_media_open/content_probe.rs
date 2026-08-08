//! Runtime proof для yt-dlp resource-а с неполной extractor track metadata.
//!
//! Модуль не выбирает candidate и не открывает transport. Он принимает уже
//! опубликованные demux tracks, проверяет exact absence/declared evidence,
//! decoder capabilities и hard playback policy, а затем возвращает typed actual
//! evidence без raw locator/identity.

use codec_core::VideoCodec;
use media_core::{TrackInfo, TrackKind};
use web_media_core::{
    AudioTrackDescriptor, ContentProbedDescriptor, ContentProbedTrackEvidence, DynamicRange,
    NormalizedCodec, RawCodecIdentity, VideoTrackDescriptor,
};
use web_media_playback_plan::{PlaybackSelectionPolicy, ResolvedVideoPolicyRejection};

use super::catalog_capabilities::AppCatalogCapabilityProbe;

/// Policy-aware adapter только для HDS `ContentProbed` rendition discovery.
///
/// Базовый catalog probe остаётся capability-only boundary для declared
/// adaptive manifests. Этот wrapper добавляет outer `Absent`/`Declared`
/// correspondence и hard runtime policy лишь там, где exact rendition
/// codec/color стали известны после provider-owned content probe.
pub(super) struct ContentProbedHdsCapabilityProbe<'probe> {
    capability_probe: &'probe AppCatalogCapabilityProbe,
    descriptor: &'probe ContentProbedDescriptor,
    playback_policy: &'probe PlaybackSelectionPolicy,
}

impl<'probe> ContentProbedHdsCapabilityProbe<'probe> {
    /// Связывает immutable capabilities и policy на время одного HDS discovery.
    pub(super) const fn new(
        capability_probe: &'probe AppCatalogCapabilityProbe,
        descriptor: &'probe ContentProbedDescriptor,
        playback_policy: &'probe PlaybackSelectionPolicy,
    ) -> Self {
        Self {
            capability_probe,
            descriptor,
            playback_policy,
        }
    }
}

impl web_media_hds::HdsRenditionCapabilityProbe for ContentProbedHdsCapabilityProbe<'_> {
    fn check_coupled_av(
        &self,
        video: &TrackInfo,
        audio: &TrackInfo,
    ) -> Result<(), web_media_hds::HdsRenditionCapabilityRejection> {
        verify_video_correspondence(self.descriptor.video(), Some(video))
            .map_err(|_| web_media_hds::HdsRenditionCapabilityRejection)?;
        verify_audio_correspondence(self.descriptor.audio(), Some(audio))
            .map_err(|_| web_media_hds::HdsRenditionCapabilityRejection)?;
        <AppCatalogCapabilityProbe as web_media_hds::HdsRenditionCapabilityProbe>::check_coupled_av(
            self.capability_probe,
            video,
            audio,
        )?;
        check_actual_video_policy(video, self.playback_policy)
            .map_err(|_| web_media_hds::HdsRenditionCapabilityRejection)
    }
}

/// Полный actual track proof одного physical resource-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ContentProbeProof {
    video: Option<VideoTrackDescriptor>,
    audio: Option<AudioTrackDescriptor>,
}

impl ContentProbeProof {
    /// Возвращает proven video, если demux его опубликовал.
    pub(super) const fn video(&self) -> Option<&VideoTrackDescriptor> {
        self.video.as_ref()
    }

    /// Возвращает proven audio, если demux его опубликовал.
    pub(super) const fn audio(&self) -> Option<&AudioTrackDescriptor> {
        self.audio.as_ref()
    }
}

/// Точная безопасная причина отказа content probe до Installed barrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(super) enum ContentProbeRejection {
    /// Demux не опубликовал ни одной media-дорожки.
    #[error("content probe не обнаружил video или audio track")]
    NoMediaTracks,
    /// Demux не подтвердил declared video presence.
    #[error("content probe не подтвердил declared video track")]
    MissingDeclaredVideo,
    /// Demux опубликовал video вопреки literal `vcodec=none`.
    #[error("content probe обнаружил video при explicit vcodec=none")]
    UnexpectedVideo,
    /// Actual video codec не совпал с declared extractor codec family.
    #[error("content probe обнаружил другой video codec, чем объявил extractor")]
    DeclaredVideoCodecMismatch,
    /// Actual video нельзя декодировать текущей capability configuration.
    #[error("content-probed video codec не поддерживается")]
    UnsupportedVideo,
    /// Demux не подтвердил declared audio presence.
    #[error("content probe не подтвердил declared audio track")]
    MissingDeclaredAudio,
    /// Demux опубликовал audio вопреки literal `acodec=none`.
    #[error("content probe обнаружил audio при explicit acodec=none")]
    UnexpectedAudio,
    /// Actual audio codec не совпал с declared extractor codec family.
    #[error("content probe обнаружил другой audio codec, чем объявил extractor")]
    DeclaredAudioCodecMismatch,
    /// Actual audio нельзя декодировать текущей capability configuration.
    #[error("content-probed audio codec не поддерживается")]
    UnsupportedAudio,
    /// Actual video нарушает hard playback selection policy.
    #[error("content-probed video отклонён playback policy: {0}")]
    VideoPolicy(ResolvedVideoPolicyRejection),
    /// Provider доказал, что ни один boundedly probed adaptive variant не playable.
    #[error("content probe не обнаружил playable adaptive variant")]
    NoPlayableAdaptiveVariant,
}

/// Доказывает actual tracks без подмены `Unknown`, `Absent` и `Declared`.
pub(super) fn prove_content_probed_tracks(
    capability_probe: &AppCatalogCapabilityProbe,
    descriptor: &ContentProbedDescriptor,
    tracks: &[TrackInfo],
    playback_policy: &PlaybackSelectionPolicy,
) -> Result<ContentProbeProof, ContentProbeRejection> {
    let video_track = tracks.iter().find(|track| track.kind == TrackKind::Video);
    let audio_track = tracks.iter().find(|track| track.kind == TrackKind::Audio);
    if video_track.is_none() && audio_track.is_none() {
        return Err(ContentProbeRejection::NoMediaTracks);
    }

    verify_video_correspondence(descriptor.video(), video_track)?;
    verify_audio_correspondence(descriptor.audio(), audio_track)?;

    let video = video_track
        .map(|track| prove_video_track(capability_probe, track, playback_policy))
        .transpose()?;
    let audio = audio_track
        .map(|track| {
            capability_probe
                .audio_descriptor(track)
                .ok_or(ContentProbeRejection::UnsupportedAudio)
        })
        .transpose()?;

    Ok(ContentProbeProof { video, audio })
}

/// Сохраняет literal absence и declared codec correspondence для video.
fn verify_video_correspondence(
    evidence: &ContentProbedTrackEvidence<VideoTrackDescriptor>,
    actual: Option<&TrackInfo>,
) -> Result<(), ContentProbeRejection> {
    match (evidence, actual) {
        (ContentProbedTrackEvidence::Declared(_), None) => {
            Err(ContentProbeRejection::MissingDeclaredVideo)
        }
        (ContentProbedTrackEvidence::Absent, Some(_)) => {
            Err(ContentProbeRejection::UnexpectedVideo)
        }
        (ContentProbedTrackEvidence::Declared(declared), Some(actual)) => {
            let actual_codec =
                normalized_track_codec(actual).ok_or(ContentProbeRejection::UnsupportedVideo)?;
            if declared.codec().kind() == actual_codec.kind() {
                Ok(())
            } else {
                Err(ContentProbeRejection::DeclaredVideoCodecMismatch)
            }
        }
        (ContentProbedTrackEvidence::Unknown, _) | (ContentProbedTrackEvidence::Absent, None) => {
            Ok(())
        }
    }
}

/// Сохраняет literal absence и declared codec correspondence для audio.
fn verify_audio_correspondence(
    evidence: &ContentProbedTrackEvidence<AudioTrackDescriptor>,
    actual: Option<&TrackInfo>,
) -> Result<(), ContentProbeRejection> {
    match (evidence, actual) {
        (ContentProbedTrackEvidence::Declared(_), None) => {
            Err(ContentProbeRejection::MissingDeclaredAudio)
        }
        (ContentProbedTrackEvidence::Absent, Some(_)) => {
            Err(ContentProbeRejection::UnexpectedAudio)
        }
        (ContentProbedTrackEvidence::Declared(declared), Some(actual)) => {
            let actual_codec =
                normalized_track_codec(actual).ok_or(ContentProbeRejection::UnsupportedAudio)?;
            if declared.codec().kind() == actual_codec.kind() {
                Ok(())
            } else {
                Err(ContentProbeRejection::DeclaredAudioCodecMismatch)
            }
        }
        (ContentProbedTrackEvidence::Unknown, _) | (ContentProbedTrackEvidence::Absent, None) => {
            Ok(())
        }
    }
}

/// Проверяет actual video decoder capability и hard selection policy.
fn prove_video_track(
    capability_probe: &AppCatalogCapabilityProbe,
    track: &TrackInfo,
    playback_policy: &PlaybackSelectionPolicy,
) -> Result<VideoTrackDescriptor, ContentProbeRejection> {
    let descriptor = capability_probe
        .video_descriptor(track)
        .ok_or(ContentProbeRejection::UnsupportedVideo)?;
    check_actual_video_policy(track, playback_policy)?;
    Ok(descriptor)
}

/// Проверяет hard policy по actual codec/color evidence без capability lookup-а.
fn check_actual_video_policy(
    track: &TrackInfo,
    playback_policy: &PlaybackSelectionPolicy,
) -> Result<(), ContentProbeRejection> {
    let codec = VideoCodec::from_container_codec_id(&track.codec_id)
        .ok_or(ContentProbeRejection::UnsupportedVideo)?;
    let dynamic_range = track
        .video
        .as_ref()
        .and_then(|video| video.color.as_ref())
        .map(|color| {
            if color.requires_hdr_processing() {
                DynamicRange::Hdr
            } else {
                DynamicRange::Sdr
            }
        });
    playback_policy
        .check_resolved_video(codec, dynamic_range)
        .map_err(ContentProbeRejection::VideoPolicy)
}

/// Нормализует container codec-id тем же neutral parser-ом, что extractor codecs.
fn normalized_track_codec(track: &TrackInfo) -> Option<NormalizedCodec> {
    RawCodecIdentity::new(track.codec_id.clone())
        .ok()
        .map(NormalizedCodec::parse)
}

#[cfg(test)]
mod tests {
    use audio::AudioDecodeCapabilitySnapshot;
    use codec_core::{
        ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoColorMetadata,
        VideoDisplayOrientation,
    };
    use media_core::{TrackId, VideoPacketFraming, VideoTrackMetadata};
    use web_media_core::{
        ContainerFamily, ContainerIdentity, ContentProbedVideoHints, NormalizedTransport,
        PreferredHeightPolicy, RawContainerIdentity, RawTransportIdentity,
    };
    use web_media_playback_plan::HdrSelectionPolicy;

    use super::*;

    /// Собирает stable ContentProbed MP4 descriptor для correspondence tests.
    fn descriptor(
        video: ContentProbedTrackEvidence<VideoTrackDescriptor>,
        audio: ContentProbedTrackEvidence<AudioTrackDescriptor>,
    ) -> ContentProbedDescriptor {
        ContentProbedDescriptor::new(
            NormalizedTransport::parse(
                RawTransportIdentity::new("https").expect("fixture transport"),
            ),
            ContainerIdentity::parse(
                None,
                Some(RawContainerIdentity::new("mp4").expect("fixture container")),
            ),
            ContainerFamily::IsoBmff,
            video,
            audio,
            ContentProbedVideoHints::new(None, None, None, None, DynamicRange::Unknown),
        )
        .expect("fixture ContentProbed descriptor")
    }

    /// Создаёт actual demux video track с optional authoritative color metadata.
    fn video_track(codec_id: &str, color: Option<VideoColorMetadata>) -> TrackInfo {
        TrackInfo {
            id: TrackId::new(1),
            kind: TrackKind::Video,
            codec_id: codec_id.to_owned(),
            codec_private: None,
            time_base: None,
            duration: None,
            sample_rate: None,
            channels: None,
            video: Some(VideoTrackMetadata {
                packet_framing: VideoPacketFraming::Unspecified,
                coded_width: Some(1920),
                coded_height: Some(1080),
                profile: None,
                bit_depth: None,
                chroma: None,
                color,
                orientation: VideoDisplayOrientation::Identity,
            }),
        }
    }

    /// Создаёт hard policy, которая допускает H.264, но не HDR.
    fn sdr_h264_policy() -> PlaybackSelectionPolicy {
        PlaybackSelectionPolicy::new(
            HdrSelectionPolicy::SdrOnly,
            vec![VideoCodec::H264],
            PreferredHeightPolicy::NoPreference,
            vec![ContainerFamily::IsoBmff],
        )
        .expect("fixture playback policy")
    }

    #[test]
    fn declared_codec_family_must_match_actual_demux_codec() {
        let declared_h264 = VideoTrackDescriptor::new(
            NormalizedCodec::parse(
                RawCodecIdentity::new("avc1.640028").expect("fixture declared codec"),
            ),
            None,
            None,
            None,
            None,
            DynamicRange::Unknown,
        );
        let actual_vp9 = video_track("V_VP9", None);
        let probe = AppCatalogCapabilityProbe::new(
            capability_core::SystemCapabilities::empty(1),
            AudioDecodeCapabilitySnapshot::empty(),
        );

        assert_eq!(
            prove_content_probed_tracks(
                &probe,
                &descriptor(
                    ContentProbedTrackEvidence::Declared(declared_h264),
                    ContentProbedTrackEvidence::Absent,
                ),
                &[actual_vp9],
                &sdr_h264_policy(),
            ),
            Err(ContentProbeRejection::DeclaredVideoCodecMismatch)
        );
    }

    #[test]
    fn actual_pq_color_metadata_is_rejected_by_sdr_only_policy() {
        let pq = VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            TransferFunction::Pq,
            None,
        );
        let actual_h264_hdr = video_track("V_MPEG4/ISO/AVC", Some(pq));

        assert_eq!(
            check_actual_video_policy(&actual_h264_hdr, &sdr_h264_policy()),
            Err(ContentProbeRejection::VideoPolicy(
                ResolvedVideoPolicyRejection::HdrExcluded
            ))
        );
    }

    #[test]
    fn hds_discovery_adapter_enforces_outer_declared_codec_correspondence() {
        let declared_vp9 = VideoTrackDescriptor::new(
            NormalizedCodec::parse(
                RawCodecIdentity::new("vp09.00.10.08").expect("fixture declared codec"),
            ),
            None,
            None,
            None,
            None,
            DynamicRange::Unknown,
        );
        let descriptor = descriptor(
            ContentProbedTrackEvidence::Declared(declared_vp9),
            ContentProbedTrackEvidence::Unknown,
        );
        let actual_h264 = video_track("V_MPEG4/ISO/AVC", None);
        let actual_aac = TrackInfo {
            id: TrackId::new(2),
            kind: TrackKind::Audio,
            codec_id: "A_AAC".to_owned(),
            codec_private: None,
            time_base: None,
            duration: None,
            sample_rate: Some(48_000),
            channels: Some(2),
            video: None,
        };
        let probe = AppCatalogCapabilityProbe::new(
            capability_core::SystemCapabilities::empty(1),
            AudioDecodeCapabilitySnapshot::empty(),
        );
        let policy = sdr_h264_policy();
        let adapter = ContentProbedHdsCapabilityProbe::new(&probe, &descriptor, &policy);

        assert_eq!(
            <ContentProbedHdsCapabilityProbe<'_> as web_media_hds::HdsRenditionCapabilityProbe>::check_coupled_av(
                &adapter,
                &actual_h264,
                &actual_aac,
            ),
            Err(web_media_hds::HdsRenditionCapabilityRejection)
        );
    }
}
