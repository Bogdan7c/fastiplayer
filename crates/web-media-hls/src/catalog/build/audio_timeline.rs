//! Изоляция версий звука, чья временная шкала несовместима с видео их audio group.
//!
//! Владелец решения — каталог: только он видит одновременно proofs видео и всех audio
//! renditions. Правило (решение владельца, backlog #20 `user/ac3-eac3-support/`):
//! - небольшое расхождение длительностей (до одного сегмента, см. `catalog/timeline.rs`)
//!   допустимо — runtime сопоставляет компоненты по времени;
//! - не-default версия, несовместимая ни с одним видео своей group, удаляется из каталога
//!   (sibling rejection с точной причиной + warning в логе), открытие продолжается;
//! - несовместимость default-пары (выбранное видео + выбранная версия звука) фатальна при
//!   любой `HlsProviderDefaultAudioPolicy`: это не «неподдерживаемый кодек», который можно
//!   молча опустить, а конфликт самого источника — молча играть без звука нельзя.

use hls_playlist_core::MasterPlaylist;

use super::{AudioOrigin, PendingAudio, PendingVideo, reject_child};
use crate::catalog::{
    HlsCatalogBuildError, HlsCatalogChildId, HlsCatalogSiblingRejection,
    HlsCatalogSiblingRejectionReason, HlsTimelineCompatibility,
};

/// Default-пара provider-а: индекс выбранного variant-а и (если есть) выбранной rendition.
#[derive(Clone, Copy)]
pub(super) struct DefaultAudioPair {
    pub(super) variant_index: usize,
    pub(super) rendition_index: Option<usize>,
}

/// Удаляет из `audios` версии, несовместимые по времени с видео своей group.
///
/// Видео-строки не меняются: шкала видео — эталон, с которым сверяется звук.
pub(super) fn isolate_timeline_mismatched_audio(
    master: &MasterPlaylist,
    videos: &[PendingVideo],
    audios: &mut Vec<PendingAudio>,
    default_pair: DefaultAudioPair,
    selected_children: &[HlsCatalogChildId],
    rejections: &mut Vec<HlsCatalogSiblingRejection>,
) -> Result<(), HlsCatalogBuildError> {
    let mut verdicts = Vec::with_capacity(audios.len());
    for audio in audios.iter() {
        let verdict = timeline_verdict(master, videos, audio, default_pair)?;
        if let Some(reason) = verdict {
            reject_child(audio.child, reason, selected_children, rejections)?;
        }
        verdicts.push(verdict);
    }
    let mut verdicts = verdicts.into_iter();
    audios.retain(|_| verdicts.next().flatten().is_none());
    Ok(())
}

/// `None` — строка остаётся; `Some(reason)` — версия несовместима и будет удалена.
fn timeline_verdict(
    master: &MasterPlaylist,
    videos: &[PendingVideo],
    audio: &PendingAudio,
    default_pair: DefaultAudioPair,
) -> Result<Option<HlsCatalogSiblingRejectionReason>, HlsCatalogBuildError> {
    // Audio-only variant не входит в audio group и с видео не сопрягается.
    let AudioOrigin::Rendition(rendition_index) = audio.origin else {
        return Ok(None);
    };
    if default_pair.rendition_index == Some(rendition_index)
        && let Some(default_video) = videos
            .iter()
            .find(|video| video.variant_index == default_pair.variant_index)
    {
        return match mismatch_reason(default_video.alignment.compatibility_with(&audio.alignment)) {
            Some(reason) => Err(HlsCatalogBuildError::ProviderDefaultRejected { reason }),
            None => Ok(None),
        };
    }
    let group: &str = &master.renditions[rendition_index].group_id;
    let mut first_mismatch = None;
    for video in videos
        .iter()
        .filter(|video| master.variants[video.variant_index].audio_group.as_deref() == Some(group))
    {
        match mismatch_reason(video.alignment.compatibility_with(&audio.alignment)) {
            // Достаточно одного совместимого видео: пару с ним и опубликует edge.
            None => return Ok(None),
            Some(reason) => {
                first_mismatch.get_or_insert(reason);
            }
        }
    }
    // Нет видео в group — сверять не с чем, прежнее поведение (строка без edges).
    Ok(first_mismatch)
}

const fn mismatch_reason(
    compatibility: HlsTimelineCompatibility,
) -> Option<HlsCatalogSiblingRejectionReason> {
    match compatibility {
        HlsTimelineCompatibility::Compatible => None,
        HlsTimelineCompatibility::TotalDurationMismatch => {
            Some(HlsCatalogSiblingRejectionReason::AudioTimelineDurationMismatch)
        }
        HlsTimelineCompatibility::DiscontinuityLayoutMismatch => {
            Some(HlsCatalogSiblingRejectionReason::AudioTimelineDiscontinuityMismatch)
        }
    }
}
