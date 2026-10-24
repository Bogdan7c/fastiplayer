//! Проверки текстур интерфейса (сессия 02a, причина 2 падения пересоздания рендера).
//!
//! Изменения текстур `EguiFrameContent::Fresh` — состояние рендера, а не содержимое кадра:
//! рендер обязан применить их и в отклонённом кадре, а частичную правку неизвестной текстуры —
//! не паниковать, а сообщить типизированной причиной, чтобы приложение прислало полный набор.

use egui::epaint::{ImageDelta, TextureId};
use egui::{Color32, ColorImage, TextureOptions, TexturesDelta};

use crate::{
    AcquiredSurfaceFrame, EguiFrameContent, RenderFrameDropReason, RenderFrameOutcome,
    RenderScreenDescriptor, RenderShell,
};

use super::{
    ConformanceCheck, ConformanceViolation, RenderShellConformanceHarness, create_renderer,
    expect_presented, frame_input, render_with_acquired_surface,
};

/// Текстура проверок: id, которого у свежего рендера заведомо нет (атлас шрифтов — `Managed(0)`).
const CONFORMANCE_UI_TEXTURE: TextureId = TextureId::Managed(7001);

/// Размер целой загрузки проверочной текстуры (частичная правка помещается внутрь).
const CONFORMANCE_UI_TEXTURE_SIZE: [usize; 2] = [4, 4];

/// Проверка 10: частичная правка неизвестной текстуры — `Dropped(UiTexturesOutOfSync)` без
/// паники; затем кадр с целой загрузкой показан, и следующая частичная правка тоже.
pub fn check_partial_edit_of_unknown_ui_texture_is_reported(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<(), ConformanceViolation> {
    let check = ConformanceCheck::PartialEditOfUnknownUiTextureIsReported;
    let screen = harness.screen();
    let mut renderer = create_renderer(harness)?;

    let refused = render_with_acquired_surface(
        renderer.as_mut(),
        check,
        screen,
        None,
        fresh_ui_with_textures(partial_edit_delta()),
    )?;
    if refused != RenderFrameOutcome::Dropped(RenderFrameDropReason::UiTexturesOutOfSync) {
        return Err(ConformanceViolation::new(
            check,
            format!(
                "partial edit of an unknown UI texture must drop as UiTexturesOutOfSync, got {refused:?}"
            ),
        ));
    }

    let full = render_with_acquired_surface(
        renderer.as_mut(),
        check,
        screen,
        None,
        fresh_ui_with_textures(full_upload_delta()),
    )?;
    expect_presented(check, "frame with the full UI texture upload", &full)?;
    let edited = render_with_acquired_surface(
        renderer.as_mut(),
        check,
        screen,
        None,
        fresh_ui_with_textures(partial_edit_delta()),
    )?;
    expect_presented(check, "partial edit of an uploaded UI texture", &edited)
}

/// Проверка 11: изменения текстур применяются и в кадре, отклонённом из-за чужого билета.
///
/// Целая загрузка в кадре с чужим билетом (рендер отклоняет его), а следующая частичная правка
/// с честным билетом показана. `free` с чужим билетом тоже применён: правка после него —
/// снова `UiTexturesOutOfSync`.
pub fn check_ui_textures_apply_on_rejected_frames(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<(), ConformanceViolation> {
    let check = ConformanceCheck::UiTexturesApplyOnRejectedFrames;
    let screen = harness.screen();
    let mut renderer = create_renderer(harness)?;

    render_with_foreign_ticket(renderer.as_mut(), screen, full_upload_delta());
    let edited = render_with_acquired_surface(
        renderer.as_mut(),
        check,
        screen,
        None,
        fresh_ui_with_textures(partial_edit_delta()),
    )?;
    expect_presented(
        check,
        "partial edit after a full upload in a rejected frame",
        &edited,
    )?;

    render_with_foreign_ticket(renderer.as_mut(), screen, free_delta());
    let after_free = render_with_acquired_surface(
        renderer.as_mut(),
        check,
        screen,
        None,
        fresh_ui_with_textures(partial_edit_delta()),
    )?;
    if after_free != RenderFrameOutcome::Dropped(RenderFrameDropReason::UiTexturesOutOfSync) {
        return Err(ConformanceViolation::new(
            check,
            format!(
                "free in a rejected frame must be applied: a later partial edit must drop as \
                 UiTexturesOutOfSync, got {after_free:?}"
            ),
        ));
    }
    Ok(())
}

/// Отдаёт изменения текстур в кадре с чужим билетом (рендер обязан его отклонить).
///
/// Итог кадра здесь не проверяется: отказ по чужому билету закрепляет проверка 1, а эта
/// проверка — только то, что изменения текстур применены, каким бы ни был итог.
fn render_with_foreign_ticket(
    renderer: &mut dyn RenderShell,
    screen: RenderScreenDescriptor,
    textures_delta: TexturesDelta,
) {
    let _outcome = renderer.render_frame(frame_input(
        AcquiredSurfaceFrame::unowned_for_conformance(),
        screen,
        None,
        fresh_ui_with_textures(textures_delta),
    ));
}

/// Свежий интерфейс без примитивов, только с изменениями текстур.
fn fresh_ui_with_textures(textures_delta: TexturesDelta) -> EguiFrameContent {
    EguiFrameContent::Fresh {
        paint_jobs: Vec::new(),
        textures_delta,
    }
}

/// Целая загрузка проверочной текстуры.
fn full_upload_delta() -> TexturesDelta {
    let mut delta = TexturesDelta::default();
    delta.push(
        CONFORMANCE_UI_TEXTURE,
        ImageDelta::full(
            ColorImage::filled(CONFORMANCE_UI_TEXTURE_SIZE, Color32::RED),
            TextureOptions::NEAREST,
        ),
    );
    delta
}

/// Частичная правка 2×2 внутри проверочной текстуры.
fn partial_edit_delta() -> TexturesDelta {
    let mut delta = TexturesDelta::default();
    delta.push(
        CONFORMANCE_UI_TEXTURE,
        ImageDelta::partial(
            [1, 1],
            ColorImage::filled([2, 2], Color32::GREEN),
            TextureOptions::NEAREST,
        ),
    );
    delta
}

/// Освобождение проверочной текстуры.
fn free_delta() -> TexturesDelta {
    let mut delta = TexturesDelta::default();
    delta.free(CONFORMANCE_UI_TEXTURE);
    delta
}
