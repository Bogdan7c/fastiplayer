//! Синхронизация текстур интерфейса между egui и рендером (сессия 02a, причина 2 падения).
//!
//! # Зачем
//!
//! egui отдаёт приложению только ИЗМЕНЕНИЯ текстур с прошлого кадра (`TexturesDelta`): новый
//! глиф в атласе шрифтов — это частичная правка уже загруженной текстуры. Рендер, созданный
//! заново (пересоздание по настройкам, откат после потери устройства), этих текстур не видел,
//! и частичная правка ему не к чему применить (egui-wgpu на ней паниковал). Кроме того, кадр,
//! который не дошёл до рендера (нет буфера swapchain), уносил свою дельту в никуда, и рендер
//! навсегда отставал от egui.
//!
//! # Что делает [`UiTextureSync`]
//!
//! Один экземпляр на `egui::Context` (владелец — UI-рантайм приложения):
//! - [`UiTextureSync::accept_ui_pass_delta`] принимает дельту каждого завершённого UI-прохода
//!   и копит её, пока она не уйдёт в рендер. Поэтому кадр без показа ничего не теряет: его
//!   дельта (включая `free`) просто уедет со следующим кадром;
//! - [`UiTextureSync::take_delta_for_renderer`] отдаёт накопленное ровно перед
//!   `RenderShell::render_frame`; если запрошен полный набор, добавляет его поверх;
//! - [`UiTextureSync::request_full_set`] — «новый рендер ничего не знает» или «рендер сообщил,
//!   что текстуры рассинхронизированы» ([`UiTextureFullSetReason`]).
//!
//! # Полный набор
//!
//! - Атлас шрифтов (`TextureId::default()`): текущее полное изображение берётся у самого egui
//!   (`Context::fonts(|fonts| fonts.image())`) только в момент запроса — своей копии атласа
//!   синхронизатор не держит;
//! - остальные текстуры (картинки приложения через `Context::load_texture` и т.п.): egui их
//!   пикселей не хранит, поэтому синхронизатор ведёт CPU-копии по каждой принятой дельте
//!   (целая — заменяет, частичная — дописывается, `free` — удаляет). Сегодня у плеера таких
//!   текстур нет, но механизм обязан работать для них (переиспользование в Fasticut).
//!
//! Целые дельты полного набора кладутся через `TexturesDelta::push`, который отбрасывает
//! накопленные частичные правки тех же id: полное изображение их уже содержит.

use std::collections::HashMap;
use std::mem;
use std::sync::Arc;

use egui::TextureOptions;
use egui::epaint::{ImageData, ImageDelta, TextureAtlas, TextureId};
use egui::{ColorImage, TexturesDelta};

/// Почему рендеру нужен полный набор текстур интерфейса, а не только изменения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiTextureFullSetReason {
    /// В слот поставлен новый рендер (пересоздание или откат): его набор текстур пуст.
    RendererInstalled,

    /// Рендер отклонил кадр с [`crate::RenderFrameDropReason::UiTexturesOutOfSync`]: частичная
    /// правка пришла для текстуры, которой у него нет.
    RendererReportedMissingTexture,
}

/// CPU-копия текстуры интерфейса, пиксели которой egui не хранит.
///
/// `Arc` — тот же, что ушёл в дельте целой загрузки: копия ничего не стоит, пока не пришла
/// частичная правка (тогда `Arc::make_mut` копирует только если дельта ещё жива у рендера).
#[derive(Debug, Clone)]
struct RetainedUiTexture {
    /// Текущее полное изображение текстуры.
    image: Arc<ColorImage>,

    /// Параметры выборки последней дельты (частичная правка тоже может их сменить).
    options: TextureOptions,
}

/// Синхронизатор текстур интерфейса: копит недоставленные изменения и собирает полный набор.
#[derive(Debug, Default)]
pub struct UiTextureSync {
    /// Изменения, которые ещё не ушли в рендер (сохраняют порядок и `free`).
    undelivered: TexturesDelta,

    /// CPU-копии живых текстур, кроме атласа шрифтов.
    retained_textures: HashMap<TextureId, RetainedUiTexture>,

    /// Запрошен ли полный набор (и почему) — исполняется ближайшим `take_delta_for_renderer`.
    full_set_request: Option<UiTextureFullSetReason>,

    /// Был ли хоть один принятый UI-проход: до первого `run_ui` у egui ещё нет шрифтов, и
    /// атлас для полного набора брать неоткуда.
    ui_pass_accepted: bool,
}

impl UiTextureSync {
    /// Пустой синхронизатор: ни одного прохода, полный набор не запрошен.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Принимает изменения текстур завершённого UI-прохода (`FullOutput::textures_delta`).
    ///
    /// Ведёт CPU-копии не-шрифтовых текстур и копит изменения до доставки в рендер. Дельта
    /// забирается целиком (после вызова она пуста, egui не будет ругаться на её уничтожение).
    pub fn accept_ui_pass_delta(&mut self, ui_pass_delta: TexturesDelta) {
        self.ui_pass_accepted = true;
        self.track_retained_textures(&ui_pass_delta);
        // `append` сохраняет смысл egui: `free` нового прохода убирает накопленные загрузки этого
        // id, целая загрузка отбрасывает прежние частичные.
        self.undelivered.append(ui_pass_delta);
    }

    /// Запрашивает полный набор текстур у ближайшего кадра со свежим интерфейсом.
    ///
    /// Повтор до исполнения не дублирует набор: запоминается последняя причина.
    pub fn request_full_set(&mut self, reason: UiTextureFullSetReason) {
        self.full_set_request = Some(reason);
    }

    /// Ждёт ли синхронизатор кадра, чтобы отправить полный набор (и почему).
    #[must_use]
    pub fn pending_full_set(&self) -> Option<UiTextureFullSetReason> {
        self.full_set_request
    }

    /// Отдаёт изменения для `RenderShell::render_frame` этого кадра.
    ///
    /// Это накопленные с прошлой доставки изменения; если запрошен полный набор — плюс целые
    /// дельты атласа шрифтов (текущее изображение `egui_context`) и всех живых CPU-копий.
    ///
    /// `egui_context` — тот же контекст, чьи проходы принимал [`Self::accept_ui_pass_delta`].
    /// До первого принятого прохода у egui нет шрифтов: запрос полного набора тогда остаётся
    /// ждать (в этом состоянии рендеру и нечего показывать).
    pub fn take_delta_for_renderer(&mut self, egui_context: &egui::Context) -> TexturesDelta {
        let mut renderer_delta = mem::take(&mut self.undelivered);
        if self.full_set_request.is_some() && self.ui_pass_accepted {
            self.full_set_request = None;
            self.push_full_set(&mut renderer_delta, egui_context);
        }
        renderer_delta
    }

    /// Сколько не-шрифтовых текстур синхронизатор может восстановить (диагностика, тесты).
    #[must_use]
    pub fn retained_texture_count(&self) -> usize {
        self.retained_textures.len()
    }

    /// Добавляет целые дельты всех живых текстур; они перекрывают частичные правки тех же id.
    fn push_full_set(&self, renderer_delta: &mut TexturesDelta, egui_context: &egui::Context) {
        let font_atlas = egui_context.fonts(|fonts| fonts.image());
        renderer_delta.push(
            TextureId::default(),
            ImageDelta::full(font_atlas, TextureAtlas::texture_options()),
        );
        for (texture_id, retained) in &self.retained_textures {
            renderer_delta.push(
                *texture_id,
                ImageDelta::full(ImageData::Color(retained.image.clone()), retained.options),
            );
        }
    }

    /// Обновляет CPU-копии по дельте прохода (атлас шрифтов не копируется — его держит egui).
    fn track_retained_textures(&mut self, ui_pass_delta: &TexturesDelta) {
        for (texture_id, image_deltas) in &ui_pass_delta.set {
            if *texture_id == TextureId::default() {
                continue;
            }
            for image_delta in image_deltas {
                self.apply_to_retained_copy(*texture_id, image_delta);
            }
        }
        // `free` применяется после `set` (как у рендера: «освободить после рисования»).
        for texture_id in &ui_pass_delta.free {
            self.retained_textures.remove(texture_id);
        }
    }

    /// Применяет одну дельту к CPU-копии текстуры.
    fn apply_to_retained_copy(&mut self, texture_id: TextureId, image_delta: &ImageDelta) {
        let ImageData::Color(delta_image) = &image_delta.image;
        let Some(position) = image_delta.pos else {
            self.retained_textures.insert(
                texture_id,
                RetainedUiTexture {
                    image: delta_image.clone(),
                    options: image_delta.options,
                },
            );
            return;
        };
        let Some(retained) = self.retained_textures.get_mut(&texture_id) else {
            // Частичная правка текстуры, целой загрузки которой синхронизатор не видел: так
            // бывает, только если он подключён не с первого прохода. Восстановить её нечем;
            // рендер, которому она нужна, сообщит о рассинхронизации.
            return;
        };
        retained.options = image_delta.options;
        if patch_color_image(Arc::make_mut(&mut retained.image), position, delta_image).is_err() {
            // Правка не помещается в известное изображение — копии больше нельзя доверять.
            self.retained_textures.remove(&texture_id);
        }
    }
}

impl Drop for UiTextureSync {
    fn drop(&mut self) {
        // Синхронизатор уходит вместе с UI-рантаймом (выход, suspend): недоставленные
        // изменения больше некому применять. egui в debug-сборке паникует на уничтожении
        // непустой `TexturesDelta`, поэтому отказ от них — явный.
        self.undelivered.clear();
    }
}

/// Частичная правка выходит за границы изображения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PatchOutOfBounds;

/// Дописывает `patch` в `target` с позиции `[x, y]` (строка за строкой, без паник).
fn patch_color_image(
    target: &mut ColorImage,
    [x, y]: [usize; 2],
    patch: &ColorImage,
) -> Result<(), PatchOutOfBounds> {
    let [target_width, target_height] = target.size;
    let [patch_width, patch_height] = patch.size;
    let fits_horizontally = x
        .checked_add(patch_width)
        .is_some_and(|right| right <= target_width);
    let fits_vertically = y
        .checked_add(patch_height)
        .is_some_and(|bottom| bottom <= target_height);
    if !fits_horizontally || !fits_vertically {
        return Err(PatchOutOfBounds);
    }
    for row in 0..patch_height {
        let source_start = row * patch_width;
        let target_start = (y + row) * target_width + x;
        let source_row = patch
            .pixels
            .get(source_start..source_start + patch_width)
            .ok_or(PatchOutOfBounds)?;
        let target_row = target
            .pixels
            .get_mut(target_start..target_start + patch_width)
            .ok_or(PatchOutOfBounds)?;
        target_row.copy_from_slice(source_row);
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/ui_textures.rs"]
mod tests;
