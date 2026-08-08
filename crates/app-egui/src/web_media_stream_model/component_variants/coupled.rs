//! Safe projection неделимых A/V rendition rows.
//!
//! Этот модуль не создаёт независимые video/audio axes: одна строка всегда
//! сохраняет provider-owned связь двух track descriptors и одного exact identity.

use std::sync::Arc;

use web_media_core::{CoupledComponentVariant, CoupledVariantExactIdentity};

use super::{
    ComponentVariantInstallationError, WebMediaAudioComponentVariantPresentation,
    WebMediaComponentVariantAxisKind, WebMediaVideoComponentVariantPresentation,
    audio_track_presentation, video_track_presentation,
};

/// Safe coupled axis с explicit active row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebMediaCoupledComponentVariantAxis {
    pub(crate) active_index: usize,
    pub(crate) variants: Arc<[WebMediaCoupledComponentVariantPresentation]>,
}

/// Только безопасные video/audio metadata одной неделимой rendition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebMediaCoupledComponentVariantPresentation {
    pub(crate) video: WebMediaVideoComponentVariantPresentation,
    pub(crate) audio: WebMediaAudioComponentVariantPresentation,
}

/// Строит coupled projection без копирования exact/semantic identities в UI model.
pub(super) fn coupled_axis(
    variants: &[CoupledComponentVariant],
    active_identity: &CoupledVariantExactIdentity,
) -> Result<WebMediaCoupledComponentVariantAxis, ComponentVariantInstallationError> {
    let active_index = variants
        .iter()
        .position(|variant| variant.exact_identity() == active_identity)
        .ok_or(ComponentVariantInstallationError::ActiveVariantMissing {
            axis: WebMediaComponentVariantAxisKind::Coupled,
        })?;
    let variants = variants
        .iter()
        .map(|variant| WebMediaCoupledComponentVariantPresentation {
            video: video_track_presentation(variant.video()),
            audio: audio_track_presentation(variant.audio()),
        })
        .collect::<Vec<_>>()
        .into();
    Ok(WebMediaCoupledComponentVariantAxis {
        active_index,
        variants,
    })
}
