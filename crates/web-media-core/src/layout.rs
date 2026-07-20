use crate::{
    AudioTrackDescriptor, ContainerIdentity, NormalizedTransport, VideoHeight, VideoTrackDescriptor,
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

/// Shape stream resources без provider/open semantics.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StreamLayout {
    /// Один resource содержит video и audio.
    Muxed(MuxedComponentDescriptor),
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
            Self::Separate { .. } => StreamLayoutKind::Separate,
            Self::VideoOnly(_) => StreamLayoutKind::VideoOnly,
            Self::AudioOnly(_) => StreamLayoutKind::AudioOnly,
        }
    }

    /// Возвращает video height, если layout содержит video.
    pub const fn video_height(&self) -> Option<VideoHeight> {
        match self {
            Self::Muxed(component) => component.video().height(),
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
    /// Separate A/V.
    Separate,
    /// Video-only.
    VideoOnly,
    /// Audio-only.
    AudioOnly,
}
