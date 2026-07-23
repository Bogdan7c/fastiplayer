use hls_playlist_core::{
    HlsPlaylist, HlsProfileError, MediaContainerIntent, MediaPlaylist, validate_vod_profile,
};

/// Доказательство, что структурно разобранный media playlist входит в S32 VOD profile.
pub struct ValidatedVodMediaPlaylist<'playlist> {
    media: &'playlist MediaPlaylist,
}

impl<'playlist> ValidatedVodMediaPlaylist<'playlist> {
    /// Проверяет VOD/profile semantics без I/O и player mutation.
    pub fn new(
        playlist: &'playlist HlsPlaylist,
        container_intent: Option<MediaContainerIntent>,
    ) -> Result<Self, HlsProfileError> {
        validate_vod_profile(playlist, container_intent)?;
        let HlsPlaylist::Media(media) = playlist else {
            return Err(HlsProfileError::MasterPlaylist);
        };
        Ok(Self { media })
    }

    /// Возвращает неизменяемую parsed media model после проверки.
    pub const fn media(&self) -> &'playlist MediaPlaylist {
        self.media
    }
}
