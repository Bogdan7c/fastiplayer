use crate::{
    AudioTrackDescriptor, Bitrate, ContainerFamily, ContainerIdentity, DynamicRange, FrameRate,
    NormalizedTransport, TransportFamily, VideoHeight, VideoTrackDescriptor, VideoWidth,
};

/// Компонент, содержащий только video track.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VideoComponentDescriptor {
    /// Transport value.
    transport: NormalizedTransport,
    /// Container hints.
    container: ContainerIdentity,
    /// Video track.
    video: VideoTrackDescriptor,
}

impl VideoComponentDescriptor {
    /// Создаёт video-only component.
    pub const fn new(
        transport: NormalizedTransport,
        container: ContainerIdentity,
        video: VideoTrackDescriptor,
    ) -> Self {
        Self {
            transport,
            container,
            video,
        }
    }

    /// Возвращает transport.
    pub const fn transport(&self) -> &NormalizedTransport {
        &self.transport
    }

    /// Возвращает container.
    pub const fn container(&self) -> &ContainerIdentity {
        &self.container
    }

    /// Возвращает video track.
    pub const fn video(&self) -> &VideoTrackDescriptor {
        &self.video
    }
}

/// Компонент, содержащий только audio track.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioComponentDescriptor {
    /// Transport value.
    transport: NormalizedTransport,
    /// Container hints.
    container: ContainerIdentity,
    /// Audio track.
    audio: AudioTrackDescriptor,
}

impl AudioComponentDescriptor {
    /// Создаёт audio-only component.
    pub const fn new(
        transport: NormalizedTransport,
        container: ContainerIdentity,
        audio: AudioTrackDescriptor,
    ) -> Self {
        Self {
            transport,
            container,
            audio,
        }
    }

    /// Возвращает transport.
    pub const fn transport(&self) -> &NormalizedTransport {
        &self.transport
    }

    /// Возвращает container.
    pub const fn container(&self) -> &ContainerIdentity {
        &self.container
    }

    /// Возвращает audio track.
    pub const fn audio(&self) -> &AudioTrackDescriptor {
        &self.audio
    }
}

/// Один transport/container component с video и audio.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MuxedComponentDescriptor {
    /// Transport value.
    transport: NormalizedTransport,
    /// Container hints.
    container: ContainerIdentity,
    /// Video track.
    video: VideoTrackDescriptor,
    /// Audio track.
    audio: AudioTrackDescriptor,
}

/// Доказательство одной дорожки для resource-а, чья полная форма выясняется demux-ом.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContentProbedTrackEvidence<Track> {
    /// Extractor не сообщил, присутствует ли дорожка.
    Unknown,
    /// Extractor явно сообщил отсутствие дорожки через literal `none`.
    Absent,
    /// Extractor объявил дорожку и её bounded descriptor.
    Declared(Track),
}

impl<Track> ContentProbedTrackEvidence<Track> {
    /// Возвращает `true` только для явно отсутствующей дорожки.
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Возвращает объявленную дорожку без подмены unknown/absent состояний.
    pub const fn declared(&self) -> Option<&Track> {
        match self {
            Self::Declared(track) => Some(track),
            Self::Unknown | Self::Absent => None,
        }
    }
}

/// Безопасные visual hints, которые не доказывают наличие video track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentProbedVideoHints {
    width: Option<VideoWidth>,
    height: Option<VideoHeight>,
    frame_rate: Option<FrameRate>,
    bitrate: Option<Bitrate>,
    dynamic_range: DynamicRange,
}

impl ContentProbedVideoHints {
    /// Создаёт отсутствие video hints для proven audio-only resource-а.
    pub const fn none() -> Self {
        Self {
            width: None,
            height: None,
            frame_rate: None,
            bitrate: None,
            dynamic_range: DynamicRange::Unknown,
        }
    }

    /// Создаёт bounded набор extractor hints без codec/topology утверждений.
    pub const fn new(
        width: Option<VideoWidth>,
        height: Option<VideoHeight>,
        frame_rate: Option<FrameRate>,
        bitrate: Option<Bitrate>,
        dynamic_range: DynamicRange,
    ) -> Self {
        Self {
            width,
            height,
            frame_rate,
            bitrate,
            dynamic_range,
        }
    }

    /// Возвращает optional width hint.
    pub const fn width(self) -> Option<VideoWidth> {
        self.width
    }

    /// Возвращает optional height hint.
    pub const fn height(self) -> Option<VideoHeight> {
        self.height
    }

    /// Возвращает optional frame-rate hint.
    pub const fn frame_rate(self) -> Option<FrameRate> {
        self.frame_rate
    }

    /// Возвращает optional bitrate hint.
    pub const fn bitrate(self) -> Option<Bitrate> {
        self.bitrate
    }

    /// Возвращает conservative dynamic-range hint.
    pub const fn dynamic_range(self) -> DynamicRange {
        self.dynamic_range
    }
}

/// Один physical resource, чьи реальные дорожки и кодеки доказывает content probe.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentProbedDescriptor {
    transport: NormalizedTransport,
    container: ContainerIdentity,
    probe_container: ContainerFamily,
    video: ContentProbedTrackEvidence<VideoTrackDescriptor>,
    audio: ContentProbedTrackEvidence<AudioTrackDescriptor>,
    video_hints: ContentProbedVideoHints,
}

/// Ошибка построения content-probed resource descriptor-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentProbedBuildError {
    /// Extractor одновременно объявил отсутствие video и audio.
    NoMediaTracks,
}

impl ContentProbedDescriptor {
    /// Создаёт single-resource descriptor без выдуманного codec или track shape.
    pub fn new(
        transport: NormalizedTransport,
        container: ContainerIdentity,
        probe_container: ContainerFamily,
        video: ContentProbedTrackEvidence<VideoTrackDescriptor>,
        audio: ContentProbedTrackEvidence<AudioTrackDescriptor>,
        video_hints: ContentProbedVideoHints,
    ) -> Result<Self, ContentProbedBuildError> {
        if video.is_absent() && audio.is_absent() {
            return Err(ContentProbedBuildError::NoMediaTracks);
        }
        // Explicit `vcodec=none` сильнее случайных extractor sidecar hints: descriptor
        // не должен позволять audio-only resource-у участвовать в video ranking/UI.
        let video_hints = if video.is_absent() {
            ContentProbedVideoHints::none()
        } else {
            video_hints
        };
        Ok(Self {
            transport,
            container,
            probe_container,
            video,
            audio,
            video_hints,
        })
    }

    /// Возвращает transport.
    pub const fn transport(&self) -> &NormalizedTransport {
        &self.transport
    }

    /// Возвращает исходные extractor container hints.
    pub const fn container(&self) -> &ContainerIdentity {
        &self.container
    }

    /// Возвращает container family, которой должен соответствовать content probe.
    pub const fn probe_container(&self) -> ContainerFamily {
        self.probe_container
    }

    /// Возвращает extractor evidence для video track.
    pub const fn video(&self) -> &ContentProbedTrackEvidence<VideoTrackDescriptor> {
        &self.video
    }

    /// Возвращает extractor evidence для audio track.
    pub const fn audio(&self) -> &ContentProbedTrackEvidence<AudioTrackDescriptor> {
        &self.audio
    }

    /// Возвращает visual hints, которые не являются доказательством video track.
    pub const fn video_hints(&self) -> ContentProbedVideoHints {
        self.video_hints
    }
}

impl MuxedComponentDescriptor {
    /// Создаёт muxed component, где обе дорожки принадлежат одному resource.
    pub const fn new(
        transport: NormalizedTransport,
        container: ContainerIdentity,
        video: VideoTrackDescriptor,
        audio: AudioTrackDescriptor,
    ) -> Self {
        Self {
            transport,
            container,
            video,
            audio,
        }
    }

    /// Возвращает transport.
    pub const fn transport(&self) -> &NormalizedTransport {
        &self.transport
    }

    /// Возвращает container.
    pub const fn container(&self) -> &ContainerIdentity {
        &self.container
    }

    /// Возвращает video track.
    pub const fn video(&self) -> &VideoTrackDescriptor {
        &self.video
    }

    /// Возвращает audio track.
    pub const fn audio(&self) -> &AudioTrackDescriptor {
        &self.audio
    }
}

/// Muxed HLS ladder step без declared CODECS; proof после manifest/TracksChanged.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HlsMuxedCodecDeferredDescriptor {
    transport: NormalizedTransport,
    container: ContainerIdentity,
    height: VideoHeight,
    width: Option<VideoWidth>,
    frame_rate: Option<FrameRate>,
    bitrate: Option<Bitrate>,
    dynamic_range: DynamicRange,
}

/// Ошибка построения deferred HLS descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlsMuxedCodecDeferredBuildError {
    /// Transport family не HLS.
    TransportNotHls,
}

impl HlsMuxedCodecDeferredDescriptor {
    /// Создаёт deferred muxed HLS step; transport обязан быть HLS.
    pub fn new(
        transport: NormalizedTransport,
        container: ContainerIdentity,
        height: VideoHeight,
        width: Option<VideoWidth>,
        frame_rate: Option<FrameRate>,
        bitrate: Option<Bitrate>,
        dynamic_range: DynamicRange,
    ) -> Result<Self, HlsMuxedCodecDeferredBuildError> {
        if transport.family() != TransportFamily::Hls {
            return Err(HlsMuxedCodecDeferredBuildError::TransportNotHls);
        }
        Ok(Self {
            transport,
            container,
            height,
            width,
            frame_rate,
            bitrate,
            dynamic_range,
        })
    }

    /// Возвращает transport.
    pub const fn transport(&self) -> &NormalizedTransport {
        &self.transport
    }

    /// Возвращает container hints.
    pub const fn container(&self) -> &ContainerIdentity {
        &self.container
    }

    /// Возвращает обязательную video height evidence.
    pub const fn height(&self) -> VideoHeight {
        self.height
    }

    /// Возвращает optional width evidence.
    pub const fn width(&self) -> Option<VideoWidth> {
        self.width
    }

    /// Возвращает optional frame rate evidence.
    pub const fn frame_rate(&self) -> Option<FrameRate> {
        self.frame_rate
    }

    /// Возвращает optional bitrate evidence.
    pub const fn bitrate(&self) -> Option<Bitrate> {
        self.bitrate
    }

    /// Возвращает conservative dynamic-range hint.
    pub const fn dynamic_range(&self) -> DynamicRange {
        self.dynamic_range
    }
}

/// Shape stream resources без provider/open semantics.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StreamLayout {
    /// Один resource содержит video и audio.
    Muxed(MuxedComponentDescriptor),
    /// Muxed HLS без declared CODECS; codec proof отложен до manifest open.
    HlsMuxedCodecDeferred(HlsMuxedCodecDeferredDescriptor),
    /// Один resource с неполными extractor metadata; topology доказывает demux.
    ContentProbed(ContentProbedDescriptor),
    /// Video и audio находятся в разных resources.
    Separate {
        /// Video-only component.
        video: VideoComponentDescriptor,
        /// Audio-only component.
        audio: AudioComponentDescriptor,
    },
    /// Только video resource.
    VideoOnly(VideoComponentDescriptor),
    /// Только audio resource.
    AudioOnly(AudioComponentDescriptor),
}

impl StreamLayout {
    /// Возвращает compact kind для diagnostics/static compatibility.
    pub const fn kind(&self) -> StreamLayoutKind {
        match self {
            Self::Muxed(_) => StreamLayoutKind::Muxed,
            Self::HlsMuxedCodecDeferred(_) => StreamLayoutKind::HlsMuxedCodecDeferred,
            Self::ContentProbed(_) => StreamLayoutKind::ContentProbed,
            Self::Separate { .. } => StreamLayoutKind::Separate,
            Self::VideoOnly(_) => StreamLayoutKind::VideoOnly,
            Self::AudioOnly(_) => StreamLayoutKind::AudioOnly,
        }
    }

    /// Возвращает video height, если layout содержит video.
    pub const fn video_height(&self) -> Option<VideoHeight> {
        match self {
            Self::Muxed(component) => component.video().height(),
            Self::HlsMuxedCodecDeferred(component) => Some(component.height()),
            Self::ContentProbed(component) => match component.video().declared() {
                Some(video) => video.height(),
                None => None,
            },
            Self::Separate { video, .. } | Self::VideoOnly(video) => video.video().height(),
            Self::AudioOnly(_) => None,
        }
    }

    /// Возвращает soft height для selection ranking, не выдавая hint за track proof.
    pub const fn video_height_hint(&self) -> Option<VideoHeight> {
        match self {
            Self::ContentProbed(component) if !component.video().is_absent() => {
                match component.video().declared() {
                    Some(video) => match video.height() {
                        Some(height) => Some(height),
                        None => component.video_hints().height(),
                    },
                    None => component.video_hints().height(),
                }
            }
            _ => self.video_height(),
        }
    }
}

/// Compact stream-layout identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StreamLayoutKind {
    /// Muxed A/V.
    Muxed,
    /// Muxed HLS без declared CODECS.
    HlsMuxedCodecDeferred,
    /// Single resource с runtime-probed track topology/codecs.
    ContentProbed,
    /// Separate A/V.
    Separate,
    /// Video-only.
    VideoOnly,
    /// Audio-only.
    AudioOnly,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NormalizedCodec, RawCodecIdentity, RawContainerIdentity, RawTransportIdentity};

    fn content_probed_descriptor(
        video: ContentProbedTrackEvidence<VideoTrackDescriptor>,
        audio: ContentProbedTrackEvidence<AudioTrackDescriptor>,
        hinted_height: Option<VideoHeight>,
    ) -> Result<ContentProbedDescriptor, ContentProbedBuildError> {
        ContentProbedDescriptor::new(
            NormalizedTransport::parse(RawTransportIdentity::new("https").unwrap()),
            ContainerIdentity::parse(None, Some(RawContainerIdentity::new("ogg").unwrap())),
            ContainerFamily::Ogg,
            video,
            audio,
            ContentProbedVideoHints::new(None, hinted_height, None, None, DynamicRange::Unknown),
        )
    }

    fn declared_video(height: VideoHeight) -> VideoTrackDescriptor {
        VideoTrackDescriptor::new(
            NormalizedCodec::parse(RawCodecIdentity::new("vp9").unwrap()),
            None,
            Some(height),
            None,
            None,
            DynamicRange::Unknown,
        )
    }

    #[test]
    fn content_probed_descriptor_rejects_two_explicitly_absent_tracks() {
        assert_eq!(
            content_probed_descriptor(
                ContentProbedTrackEvidence::Absent,
                ContentProbedTrackEvidence::Absent,
                None,
            ),
            Err(ContentProbedBuildError::NoMediaTracks)
        );
    }

    #[test]
    fn content_probed_descriptor_preserves_unknown_and_absent_evidence() {
        let descriptor = content_probed_descriptor(
            ContentProbedTrackEvidence::Unknown,
            ContentProbedTrackEvidence::Absent,
            None,
        )
        .unwrap();

        assert!(matches!(
            descriptor.video(),
            ContentProbedTrackEvidence::Unknown
        ));
        assert!(matches!(
            descriptor.audio(),
            ContentProbedTrackEvidence::Absent
        ));
    }

    #[test]
    fn stream_layout_height_ignores_unproven_hint_and_uses_declared_video() {
        let hinted_height = VideoHeight::new(1_080).unwrap();
        let unknown_video = StreamLayout::ContentProbed(
            content_probed_descriptor(
                ContentProbedTrackEvidence::Unknown,
                ContentProbedTrackEvidence::Absent,
                Some(hinted_height),
            )
            .unwrap(),
        );
        assert_eq!(unknown_video.video_height(), None);
        assert_eq!(unknown_video.video_height_hint(), Some(hinted_height));

        let declared_height = VideoHeight::new(720).unwrap();
        let declared_video = StreamLayout::ContentProbed(
            content_probed_descriptor(
                ContentProbedTrackEvidence::Declared(declared_video(declared_height)),
                ContentProbedTrackEvidence::Absent,
                Some(hinted_height),
            )
            .unwrap(),
        );
        assert_eq!(declared_video.video_height(), Some(declared_height));
        assert_eq!(declared_video.video_height_hint(), Some(declared_height));
    }

    #[test]
    fn explicit_video_absence_discards_stray_visual_hints() {
        let stray_height = VideoHeight::new(1_080).unwrap();
        let descriptor = content_probed_descriptor(
            ContentProbedTrackEvidence::Absent,
            ContentProbedTrackEvidence::Unknown,
            Some(stray_height),
        )
        .unwrap();
        let layout = StreamLayout::ContentProbed(descriptor);

        assert_eq!(layout.video_height(), None);
        assert_eq!(layout.video_height_hint(), None);
        let StreamLayout::ContentProbed(descriptor) = layout else {
            panic!("layout остаётся content-probed");
        };
        assert_eq!(descriptor.video_hints(), ContentProbedVideoHints::none());
    }
}
