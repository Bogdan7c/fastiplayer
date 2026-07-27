use crate::{
    AudioTrackDescriptor, Bitrate, ContainerIdentity, DynamicRange, FrameRate, NormalizedTransport,
    TransportFamily, VideoHeight, VideoTrackDescriptor, VideoWidth,
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
            Self::Separate { video, .. } | Self::VideoOnly(video) => video.video().height(),
            Self::AudioOnly(_) => None,
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
    /// Separate A/V.
    Separate,
    /// Video-only.
    VideoOnly,
    /// Audio-only.
    AudioOnly,
}
