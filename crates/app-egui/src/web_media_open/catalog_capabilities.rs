//! Immutable app composition of provider content proofs with decoder capabilities.

use audio::{
    AudioDecodeCapability, AudioDecodeCapabilitySnapshot, AudioDecodeCodecFamily,
    AudioDecodeCodecFamilyQuery,
};
use codec_core::{VideoCodec, VideoMetadataSource, resolve_video_metadata};
use media_core::TrackInfo;
use web_media_core::{
    AudioTrackDescriptor, ChannelCount, CodecFamily, CodecKind, DynamicRange, NormalizedCodec,
    RawCodecIdentity, SampleRate, VideoHeight, VideoTrackDescriptor, VideoWidth,
};

#[derive(Clone)]
pub(crate) struct AppCatalogCapabilityProbe {
    video: capability_core::SystemCapabilities,
    audio: AudioDecodeCapabilitySnapshot,
}

impl AppCatalogCapabilityProbe {
    pub(super) fn new(
        video: capability_core::SystemCapabilities,
        audio: AudioDecodeCapabilitySnapshot,
    ) -> Self {
        Self { video, audio }
    }

    pub(super) fn video_descriptor(&self, track: &TrackInfo) -> Option<VideoTrackDescriptor> {
        let codec = VideoCodec::from_container_codec_id(&track.codec_id)?;
        let mut source = VideoMetadataSource::container(codec);
        if let Some(video) = &track.video {
            source.profile = video.profile;
            source.bit_depth = video.bit_depth;
            source.chroma = video.chroma;
            source.width = video.coded_width;
            source.height = video.coded_height;
            if let Some(color) = &video.color {
                source = source.with_color(color.clone());
            }
        }
        let requirement = resolve_video_metadata(codec, Some(source), None).requirement;
        self.video.check_video_requirement(&requirement).ok()?;
        let raw = RawCodecIdentity::new(track.codec_id.clone()).ok()?;
        let normalized = NormalizedCodec::parse(raw);
        let video = track.video.as_ref();
        let width = video
            .and_then(|metadata| metadata.coded_width)
            .and_then(|width| VideoWidth::new(width).ok());
        let height = video
            .and_then(|metadata| metadata.coded_height)
            .and_then(|height| VideoHeight::new(height).ok());
        let dynamic_range = match video.and_then(|metadata| metadata.color.as_ref()) {
            Some(color) if color.requires_hdr_processing() => DynamicRange::Hdr,
            Some(_) => DynamicRange::Sdr,
            None => DynamicRange::Unknown,
        };
        Some(VideoTrackDescriptor::new(
            normalized,
            width,
            height,
            None,
            None,
            dynamic_range,
        ))
    }

    pub(super) fn audio_descriptor(&self, track: &TrackInfo) -> Option<AudioTrackDescriptor> {
        let normalized =
            NormalizedCodec::parse(RawCodecIdentity::new(track.codec_id.clone()).ok()?);
        let family = audio_family(normalized.kind())?;
        if self
            .audio
            .query(AudioDecodeCodecFamilyQuery::Known(family))
            .ok()?
            != AudioDecodeCapability::Available
        {
            return None;
        }
        Some(AudioTrackDescriptor::new(
            normalized,
            track
                .sample_rate
                .and_then(|rate| SampleRate::new(rate).ok()),
            track
                .channels
                .and_then(|channels| u16::try_from(channels).ok())
                .and_then(|channels| ChannelCount::new(channels).ok()),
            None,
            None,
        ))
    }
}

impl web_media_hls::HlsCatalogCapabilityProofPort for AppCatalogCapabilityProbe {
    fn prove_video(
        &mut self,
        track: &TrackInfo,
    ) -> Result<VideoTrackDescriptor, web_media_hls::HlsCatalogCapabilityRejection> {
        self.video_descriptor(track)
            .ok_or(web_media_hls::HlsCatalogCapabilityRejection::Unsupported)
    }

    fn prove_audio(
        &mut self,
        track: &TrackInfo,
    ) -> Result<AudioTrackDescriptor, web_media_hls::HlsCatalogCapabilityRejection> {
        self.audio_descriptor(track)
            .ok_or(web_media_hls::HlsCatalogCapabilityRejection::Unsupported)
    }
}

impl web_media_dash::DashRepresentationCapabilityProbe for AppCatalogCapabilityProbe {
    fn check_video(
        &self,
        video: &TrackInfo,
    ) -> Result<(), web_media_dash::DashRepresentationCapabilityRejection> {
        self.video_descriptor(video)
            .map(|_| ())
            .ok_or(web_media_dash::DashRepresentationCapabilityRejection)
    }

    fn check_audio(
        &self,
        audio: &TrackInfo,
    ) -> Result<(), web_media_dash::DashRepresentationCapabilityRejection> {
        self.audio_descriptor(audio)
            .map(|_| ())
            .ok_or(web_media_dash::DashRepresentationCapabilityRejection)
    }

    fn check_muxed(
        &self,
        video: &TrackInfo,
        audio: &TrackInfo,
    ) -> Result<(), web_media_dash::DashRepresentationCapabilityRejection> {
        self.check_video(video)?;
        self.check_audio(audio)
    }
}

impl web_media_smooth::SmoothComponentCapabilityProbe for AppCatalogCapabilityProbe {
    fn check_video(
        &self,
        track: &TrackInfo,
    ) -> Result<(), web_media_smooth::SmoothComponentCapabilityRejection> {
        self.video_descriptor(track)
            .map(|_| ())
            .ok_or(web_media_smooth::SmoothComponentCapabilityRejection)
    }

    fn check_audio(
        &self,
        track: &TrackInfo,
    ) -> Result<(), web_media_smooth::SmoothComponentCapabilityRejection> {
        self.audio_descriptor(track)
            .map(|_| ())
            .ok_or(web_media_smooth::SmoothComponentCapabilityRejection)
    }
}

impl web_media_hds::HdsRenditionCapabilityProbe for AppCatalogCapabilityProbe {
    fn check_coupled_av(
        &self,
        video: &TrackInfo,
        audio: &TrackInfo,
    ) -> Result<(), web_media_hds::HdsRenditionCapabilityRejection> {
        self.video_descriptor(video)
            .ok_or(web_media_hds::HdsRenditionCapabilityRejection)?;
        self.audio_descriptor(audio)
            .ok_or(web_media_hds::HdsRenditionCapabilityRejection)?;
        Ok(())
    }
}

fn audio_family(kind: CodecKind) -> Option<AudioDecodeCodecFamily> {
    match kind {
        CodecKind::Known(CodecFamily::Aac | CodecFamily::IsoBmffAudio) => {
            Some(AudioDecodeCodecFamily::Aac)
        }
        CodecKind::Known(CodecFamily::Adpcm) => Some(AudioDecodeCodecFamily::Adpcm),
        CodecKind::Known(CodecFamily::Alac) => Some(AudioDecodeCodecFamily::Alac),
        CodecKind::Known(CodecFamily::Flac) => Some(AudioDecodeCodecFamily::Flac),
        CodecKind::Known(CodecFamily::Mp1) => Some(AudioDecodeCodecFamily::Mp1),
        CodecKind::Known(CodecFamily::Mp2) => Some(AudioDecodeCodecFamily::Mp2),
        CodecKind::Known(CodecFamily::Mp3) => Some(AudioDecodeCodecFamily::Mp3),
        CodecKind::Known(CodecFamily::Opus) => Some(AudioDecodeCodecFamily::Opus),
        CodecKind::Known(CodecFamily::Pcm) => Some(AudioDecodeCodecFamily::Pcm),
        CodecKind::Known(CodecFamily::Vorbis) => Some(AudioDecodeCodecFamily::Vorbis),
        _ => None,
    }
}
