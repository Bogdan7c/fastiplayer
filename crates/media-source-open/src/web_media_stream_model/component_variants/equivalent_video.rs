//! Смена версии звука там, где audio group «привязана» к своему video variant-у.
//!
//! # Зачем
//! HLS master по Apple authoring spec для Dolby-звука публикует отдельную audio group на
//! каждый кодек (`aac`, `ec3`) и отдельный `EXT-X-STREAM-INF` на каждую group, причём видео у
//! этих variant-ов одно и то же. В Topology-каталоге это два video row с одинаковой картинкой
//! и sparse compatibility: AAC совместим только с первым, E-AC-3 — только со вторым. Обычная
//! `replace_audio` (видео не трогаем) честно отвечает `IncompatibleComponentPair`.
//!
//! # Правило
//! Если точная замена невозможна именно из-за несовместимой пары, ищем другой video row с
//! той же ВИДИМОЙ картинкой (точный кодек со строкой профиля, ширина, высота, частота
//! кадров, HDR; битрейт не учитывается — у HLS variant-а он включает звук) и совместимый с запрошенным звуком.
//! Первый такой row в порядке каталога (детерминированно) образует новую пару. Если такого
//! нет — прежняя ошибка `IncompatibleComponentPair` без изменений: качество видео ради звука
//! молча не меняется (это было бы решение владельца, см. memory
//! `app-egui/web-audio-versions-menu`).

use web_media_core::{
    ComponentVariantCatalog, ComponentVariantError, ComponentVariantExactIdentity,
    ComponentVariantSelection, ComponentVariantSelectionRequest, VideoComponentVariant,
};

use super::{WebMediaVideoComponentVariantPresentation, video_track_presentation};

/// Заменяет audio, сохраняя видимое видео; см. правило в документации модуля.
pub(super) fn replace_audio_keeping_visible_video(
    catalog: &ComponentVariantCatalog,
    selection: &ComponentVariantSelection,
    requested_audio: &ComponentVariantExactIdentity,
) -> Result<ComponentVariantSelection, ComponentVariantError> {
    match selection.replace_audio(catalog, requested_audio) {
        Err(ComponentVariantError::IncompatibleComponentPair) => {}
        exact_outcome => return exact_outcome,
    }
    let ComponentVariantSelection::VideoAndAudio {
        video: current_video,
        ..
    } = selection
    else {
        return Err(ComponentVariantError::IncompatibleComponentPair);
    };
    let Some(compatibility) = catalog.compatibility() else {
        return Err(ComponentVariantError::IncompatibleComponentPair);
    };
    let current_picture = visible_picture(current_video);
    let equivalent_video = catalog
        .required_video_variants()?
        .iter()
        .find(|candidate| {
            candidate.exact_identity() != current_video.exact_identity()
                && visible_picture(candidate) == current_picture
                && candidate.track().codec() == current_video.track().codec()
                && compatibility.allows(candidate.exact_identity(), requested_audio)
        })
        .ok_or(ComponentVariantError::IncompatibleComponentPair)?;
    catalog.select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
        video: equivalent_video.exact_identity().clone(),
        audio: requested_audio.clone(),
    })
}

/// Видимые свойства картинки: безопасная проекция без битрейта.
fn visible_picture(variant: &VideoComponentVariant) -> WebMediaVideoComponentVariantPresentation {
    WebMediaVideoComponentVariantPresentation {
        bitrate: None,
        ..video_track_presentation(variant.track())
    }
}
