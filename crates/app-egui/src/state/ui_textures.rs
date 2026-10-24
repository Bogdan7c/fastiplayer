//! Доставка текстур интерфейса в рендер (сессия 02a плана `user/gles2-renderer/`).
//!
//! Владелец — `AppState`: синхронизатор живёт рядом с единственным `egui::Context` и
//! переживает замену рендера в слоте. Порядок кадра:
//! 1. UI-проход отдаёт дельту текстур синхронизатору сразу ([`AppState::accept_ui_pass_textures`]);
//!    в подготовленном кадре остаются только примитивы ([`PreparedEguiContent`]). Поэтому кадр,
//!    который не дошёл до рендера (нет буфера swapchain, план без показа), ничего не теряет:
//!    его изменения, включая `free`, уедут со следующим свежим кадром;
//! 2. непосредственно перед `RenderShell::render_frame` — уже после возможной замены рендера
//!    в этом же кадре — изменения забираются ([`AppState::egui_content_for_renderer`]);
//! 3. новый рендер в слоте или его отказ `UiTexturesOutOfSync` →
//!    [`AppState::request_full_ui_texture_set`]: ближайший свежий кадр несёт полный набор.

use render_shell_api::{EguiFrameContent, UiTextureFullSetReason, UiTextureSync};

use super::AppState;

/// Интерфейс подготовленного кадра без изменений текстур (их держит синхронизатор).
pub(crate) enum PreparedEguiContent {
    /// Свежий egui-проход: tessellated примитивы этого кадра.
    Fresh {
        /// Примитивы для рендера.
        paint_jobs: Vec<egui::epaint::ClippedPrimitive>,
    },

    /// Повтор удержанных рендером примитивов (кадр без egui-прохода).
    ReusePrevious,
}

impl AppState {
    /// Принимает изменения текстур завершённого UI-прохода (до доставки в рендер).
    pub(crate) fn accept_ui_pass_textures(&mut self, ui_pass_delta: egui::TexturesDelta) {
        self.ui_textures.accept_ui_pass_delta(ui_pass_delta);
    }

    /// Собирает интерфейс кадра для `render_frame`: свежему кадру — накопленные изменения
    /// текстур (и полный набор, если он запрошен).
    pub(crate) fn egui_content_for_renderer(
        &mut self,
        prepared: PreparedEguiContent,
    ) -> EguiFrameContent {
        egui_content_for_renderer(&mut self.ui_textures, &self.egui_ctx, prepared)
    }

    /// Просит отправить полный набор текстур интерфейса ближайшим свежим кадром.
    pub(crate) fn request_full_ui_texture_set(&mut self, reason: UiTextureFullSetReason) {
        tracing::debug!(?reason, "Full UI texture set requested for the renderer");
        self.ui_textures.request_full_set(reason);
    }
}

/// Интерфейс кадра для рендера; повтор удержанного изменений текстур не забирает — они
/// дождутся ближайшего свежего кадра.
fn egui_content_for_renderer(
    ui_textures: &mut UiTextureSync,
    egui_context: &egui::Context,
    prepared: PreparedEguiContent,
) -> EguiFrameContent {
    match prepared {
        PreparedEguiContent::Fresh { paint_jobs } => EguiFrameContent::Fresh {
            paint_jobs,
            textures_delta: ui_textures.take_delta_for_renderer(egui_context),
        },
        PreparedEguiContent::ReusePrevious => EguiFrameContent::ReusePrevious,
    }
}

#[cfg(test)]
#[path = "ui_textures/tests.rs"]
mod tests;
