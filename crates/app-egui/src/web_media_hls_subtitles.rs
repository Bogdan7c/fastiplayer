//! Secret-safe installed projection HLS subtitle renditions без transport locator-а.

use web_media_hls::HlsSubtitleRenditionDescriptor;

/// Descriptor доступного subtitle rendition, который безопасно переживает reopen/rematch.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct InstalledHlsSubtitleRendition {
    group_id: Box<str>,
    name: Box<str>,
    language: Option<Box<str>>,
    characteristics: Option<Box<str>>,
    forced: bool,
}

impl std::fmt::Debug for InstalledHlsSubtitleRendition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstalledHlsSubtitleRendition")
            .field("group_id", &self.group_id())
            .field("name", &self.name())
            .field("language", &self.language())
            .field("characteristics", &self.characteristics())
            .field("forced", &self.is_forced())
            .finish()
    }
}

impl InstalledHlsSubtitleRendition {
    /// Строит projection намеренно без URI/reference.
    pub(crate) fn from_prepared(descriptor: &HlsSubtitleRenditionDescriptor) -> Self {
        Self {
            group_id: descriptor.group_id().into(),
            name: descriptor.name().into(),
            language: descriptor.language().map(Into::into),
            characteristics: descriptor.characteristics().map(Into::into),
            forced: descriptor.is_forced(),
        }
    }

    /// Master group exact установленного rendition.
    pub(crate) fn group_id(&self) -> &str {
        &self.group_id
    }

    /// Human-facing bounded rendition name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Optional language metadata.
    pub(crate) fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Optional accessibility characteristics.
    pub(crate) fn characteristics(&self) -> Option<&str> {
        self.characteristics.as_deref()
    }

    /// RFC FORCED marker.
    pub(crate) const fn is_forced(&self) -> bool {
        self.forced
    }

    /// Test-only fixture constructor остаётся внутри descriptor owner-а.
    #[cfg(test)]
    pub(crate) fn fixture(
        group_id: &str,
        name: &str,
        language: Option<&str>,
        characteristics: Option<&str>,
        forced: bool,
    ) -> Self {
        Self {
            group_id: group_id.into(),
            name: name.into(),
            language: language.map(Into::into),
            characteristics: characteristics.map(Into::into),
            forced,
        }
    }
}
