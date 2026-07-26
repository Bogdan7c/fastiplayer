use std::fmt::Write;

use web_media_core::{
    AudioTrackDescriptor, MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES, VideoTrackDescriptor,
};

use super::HlsCatalogBuildError;

pub(super) struct SemanticKeyBuilder {
    prefix: &'static str,
    framed_fields: String,
}

impl SemanticKeyBuilder {
    pub(super) fn new(prefix: &'static str) -> Result<Self, HlsCatalogBuildError> {
        Ok(Self {
            prefix,
            framed_fields: String::new(),
        })
    }

    pub(super) fn field(&mut self, value: &[u8]) -> Result<(), HlsCatalogBuildError> {
        write!(&mut self.framed_fields, "{}:", value.len())
            .map_err(|_| HlsCatalogBuildError::SemanticIdentity)?;
        for byte in value {
            write!(&mut self.framed_fields, "{byte:02x}")
                .map_err(|_| HlsCatalogBuildError::SemanticIdentity)?;
        }
        Ok(())
    }

    pub(super) fn optional_field(
        &mut self,
        value: Option<&[u8]>,
    ) -> Result<(), HlsCatalogBuildError> {
        match value {
            Some(value) => {
                self.field(b"some")?;
                self.field(value)
            }
            None => self.field(b"none"),
        }
    }

    pub(super) fn video(
        &mut self,
        video: &VideoTrackDescriptor,
    ) -> Result<(), HlsCatalogBuildError> {
        self.field(video.codec().raw().as_str().as_bytes())?;
        let width = video.width_pixels().map(u32::to_be_bytes);
        self.optional_field(width.as_ref().map(|value| value.as_slice()))?;
        let height = video.height().map(|height| height.pixels().to_be_bytes());
        self.optional_field(height.as_ref().map(|value| value.as_slice()))?;
        let frame_rate = video.frame_rate().map(|rate| {
            let mut bytes = [0_u8; 8];
            bytes[..4].copy_from_slice(&rate.numerator().to_be_bytes());
            bytes[4..].copy_from_slice(&rate.denominator().to_be_bytes());
            bytes
        });
        self.optional_field(frame_rate.as_ref().map(|value| value.as_slice()))?;
        let bitrate = video
            .bitrate()
            .map(|rate| rate.bits_per_second().to_be_bytes());
        self.optional_field(bitrate.as_ref().map(|value| value.as_slice()))?;
        self.field(match video.dynamic_range() {
            web_media_core::DynamicRange::Sdr => b"sdr",
            web_media_core::DynamicRange::Hdr => b"hdr",
            web_media_core::DynamicRange::Unknown => b"unknown",
        })
    }

    pub(super) fn audio(
        &mut self,
        audio: &AudioTrackDescriptor,
    ) -> Result<(), HlsCatalogBuildError> {
        self.field(audio.codec().raw().as_str().as_bytes())?;
        let sample_rate = audio.sample_rate().map(|rate| rate.hertz().to_be_bytes());
        self.optional_field(sample_rate.as_ref().map(|value| value.as_slice()))?;
        let channels = audio
            .channels()
            .map(|channels| channels.get().to_be_bytes());
        self.optional_field(channels.as_ref().map(|value| value.as_slice()))?;
        let bitrate = audio
            .bitrate()
            .map(|rate| rate.bits_per_second().to_be_bytes());
        self.optional_field(bitrate.as_ref().map(|value| value.as_slice()))?;
        self.optional_field(
            audio
                .language()
                .map(|language| language.as_str().as_bytes()),
        )
    }

    pub(super) fn finish(self) -> Result<String, HlsCatalogBuildError> {
        let output_length = self
            .prefix
            .len()
            .checked_add(1)
            .and_then(|length| length.checked_add(self.framed_fields.len()))
            .ok_or(HlsCatalogBuildError::SemanticIdentity)?;
        if output_length > MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES {
            return Err(HlsCatalogBuildError::SemanticIdentity);
        }
        let mut key = String::with_capacity(output_length);
        key.push_str(self.prefix);
        key.push(':');
        key.push_str(&self.framed_fields);
        Ok(key)
    }
}
