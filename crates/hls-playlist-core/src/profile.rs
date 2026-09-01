use crate::{
    ClosedCaptionsReference, HlsKeyDeclaration, HlsKeyFormat, HlsKeyMethod, HlsPlaylist,
    HlsPlaylistType, HlsProfileError, MediaContainerIntent, MediaRenditionType,
};

/// Проверяет известные неподдерживаемые semantics для master или media topology.
pub fn validate_initial_profile(playlist: &HlsPlaylist) -> Result<(), HlsProfileError> {
    match playlist {
        HlsPlaylist::Master(master) => {
            if master.has_low_latency_semantics {
                return Err(HlsProfileError::LowLatencySemantics);
            }
            validate_common_semantics(
                master.protocol_version,
                master.has_start_offset,
                master.has_variable_substitution,
                master.has_content_steering,
            )?;
            if master.has_i_frame_variant {
                return Err(HlsProfileError::IFrameVariant);
            }
            validate_key_declarations(&master.session_keys)?;
            if master.has_session_key {
                return Err(HlsProfileError::SessionKey);
            }
            if master
                .renditions
                .iter()
                .any(|rendition| rendition.rendition_type == MediaRenditionType::Video)
                || master
                    .variants
                    .iter()
                    .any(|variant| variant.video_group.is_some())
            {
                return Err(HlsProfileError::VideoRendition);
            }
            if master
                .renditions
                .iter()
                .any(|rendition| rendition.rendition_type == MediaRenditionType::ClosedCaptions)
                || master.variants.iter().any(|variant| {
                    matches!(
                        variant.closed_captions,
                        Some(ClosedCaptionsReference::Group(_))
                    )
                })
            {
                return Err(HlsProfileError::ClosedCaptions);
            }
            if master
                .variants
                .iter()
                .any(|variant| variant.requires_output_protection)
            {
                return Err(HlsProfileError::OutputProtection);
            }
            Ok(())
        }
        HlsPlaylist::Media(media) => {
            if media.has_low_latency_semantics {
                return Err(HlsProfileError::LowLatencySemantics);
            }
            if media.i_frames_only {
                return Err(HlsProfileError::IFramesOnly);
            }
            validate_common_semantics(
                media.protocol_version,
                media.has_start_offset,
                media.has_variable_substitution,
                media.has_content_steering,
            )?;
            validate_key_declarations(&media.key_declarations)
        }
    }
}

/// Проверяет S32 initial VOD compatibility profile после структурного разбора.
pub fn validate_vod_profile(
    playlist: &HlsPlaylist,
    container_intent: Option<MediaContainerIntent>,
) -> Result<(), HlsProfileError> {
    let HlsPlaylist::Media(media) = playlist else {
        return Err(HlsProfileError::MasterPlaylist);
    };
    if !media.end_list {
        return Err(HlsProfileError::NonVod);
    }
    if media.playlist_type == Some(HlsPlaylistType::Event) {
        return Err(HlsProfileError::EventPlaylist);
    }
    validate_initial_profile(playlist)?;
    if container_intent == Some(MediaContainerIntent::FragmentedMp4)
        && media
            .segments
            .iter()
            .any(|segment| segment.initialization_map.is_none())
    {
        return Err(HlsProfileError::FragmentedMp4MapRequired);
    }
    Ok(())
}

/// Проверяет initial S33 sliding/EVENT live profile.
///
/// Live intent приходит от service descriptor-а. Эта функция намеренно не
/// выводит live только из отсутствующего `EXT-X-ENDLIST`.
pub fn validate_live_profile(
    playlist: &HlsPlaylist,
    container_intent: Option<MediaContainerIntent>,
) -> Result<(), HlsProfileError> {
    validate_live_media_profile(playlist, container_intent, false)
}

/// Проверяет очередной S33 refresh snapshot.
///
/// `EXT-X-ENDLIST` допустим только на refresh: runtime после уже принятых
/// packets явно завершит live stream через обычный terminal drain.
pub fn validate_live_refresh_profile(
    playlist: &HlsPlaylist,
    container_intent: Option<MediaContainerIntent>,
) -> Result<(), HlsProfileError> {
    validate_live_media_profile(playlist, container_intent, true)
}

fn validate_live_media_profile(
    playlist: &HlsPlaylist,
    container_intent: Option<MediaContainerIntent>,
    allow_end_list: bool,
) -> Result<(), HlsProfileError> {
    let HlsPlaylist::Media(media) = playlist else {
        return Err(HlsProfileError::MasterPlaylist);
    };
    if media.end_list && !allow_end_list {
        return Err(HlsProfileError::EndedLivePlaylist);
    }
    // EVENT остаётся live presentation: playlist только дополняется и может завершиться
    // ENDLIST на refresh-е. Явный VOD без ENDLIST не должен маскироваться live runtime-ом.
    if media.playlist_type == Some(HlsPlaylistType::Vod) {
        return Err(HlsProfileError::LivePlaylistType);
    }
    validate_initial_profile(playlist)?;
    if container_intent == Some(MediaContainerIntent::FragmentedMp4)
        && media
            .segments
            .iter()
            .any(|segment| segment.initialization_map.is_none())
    {
        return Err(HlsProfileError::FragmentedMp4MapRequired);
    }
    Ok(())
}

fn validate_common_semantics(
    protocol_version: Option<u64>,
    has_start_offset: bool,
    has_variable_substitution: bool,
    has_content_steering: bool,
) -> Result<(), HlsProfileError> {
    if protocol_version.is_some_and(|version| version > 7) {
        return Err(HlsProfileError::UnsupportedProtocolVersion);
    }
    if has_start_offset {
        return Err(HlsProfileError::StartOffset);
    }
    if has_variable_substitution {
        return Err(HlsProfileError::VariableSubstitution);
    }
    if has_content_steering {
        return Err(HlsProfileError::ContentSteering);
    }
    Ok(())
}

fn validate_key_declarations(declarations: &[HlsKeyDeclaration]) -> Result<(), HlsProfileError> {
    for key in declarations {
        match key.method {
            HlsKeyMethod::None | HlsKeyMethod::Aes128 => {}
            HlsKeyMethod::SampleAes | HlsKeyMethod::Other(_) => {
                return Err(HlsProfileError::UnsupportedEncryptionMethod);
            }
        }
        if !matches!(
            key.key_format,
            HlsKeyFormat::ImplicitIdentity | HlsKeyFormat::Identity
        ) {
            return Err(HlsProfileError::UnsupportedKeyFormat);
        }
    }
    Ok(())
}
