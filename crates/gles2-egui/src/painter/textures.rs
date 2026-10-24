//! Текстуры egui художника: кэш GL-текстур по `TextureId` и правила применения
//! `TexturesDelta` (контракт egui + наследство сессии 02a).
//!
//! - `set` применяется по порядку внутри одного id (egui 0.36 группирует дельты по id).
//! - Полная загрузка (`pos: None`) создаёт или пересоздаёт хранилище.
//! - Частичная правка (`pos: Some`) допустима только у известной текстуры и только в её
//!   границах. Иначе — не паника, а [`UiTextureUpload::OutOfSync`]: приложение пришлёт полный
//!   набор. Остальные дельты кадра всё равно применяются (как `EguiCompositor` у wgpu).
//! - `free` — только после показа кадра (или сразу, если кадр отклонён); это решает сборщик
//!   кадра, здесь только исполнение.
//! - Формат всегда RGBA8 без sRGB (egui присылает premultiplied sRGB-байты, шейдер смешивает в
//!   гамма-пространстве). Мип-уровней нет: egui-wgpu тоже игнорирует `mipmap_mode`.

use std::collections::HashMap;

use epaint::textures::{TextureFilter, TextureOptions, TextureWrapMode};
use epaint::{ImageData, ImageDelta, TextureId};

use crate::draw_gl::{DrawGl, DrawGlError, TextureSampling, UnpackRowLength};

/// Итог применения изменений текстур кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiTextureUpload {
    /// Все дельты применены.
    InSync,
    /// Частичная правка неизвестной текстуры или правка за её границами — у художника нет того,
    /// что ждёт egui. Кадр нужно отбросить и прислать полный набор текстур.
    OutOfSync {
        /// Первая (в порядке обхода) текстура, правку которой применить нельзя.
        texture_id: TextureId,
    },
}

/// Отчёт о применении `set` одного кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureApplyReport {
    /// Синхронность с egui.
    pub upload: UiTextureUpload,
    /// Полных загрузок (`glTexImage2D`).
    pub full_uploads: u32,
    /// Частичных правок (`glTexSubImage2D`).
    pub patch_uploads: u32,
    /// Полных загрузок больше `GL_MAX_TEXTURE_SIZE`: пропущены, текстуры нет (меши с ней не
    /// рисуются). Приложение должно передавать egui `max_texture_side`
    /// ([`crate::EguiPainter::max_texture_side`]).
    pub oversized_skipped: u32,
    /// Повтор (`Repeat`/`MirroredRepeat`) у текстуры с размером не степени двойки: в GLES 2.0
    /// такая текстура неполна (3.8.2), поэтому стоит `CLAMP_TO_EDGE`.
    pub npot_wrap_fallbacks: u32,
}

impl TextureApplyReport {
    fn empty() -> Self {
        Self {
            upload: UiTextureUpload::InSync,
            full_uploads: 0,
            patch_uploads: 0,
            oversized_skipped: 0,
            npot_wrap_fallbacks: 0,
        }
    }

    fn note_out_of_sync(&mut self, texture_id: TextureId) {
        if self.upload == UiTextureUpload::InSync {
            self.upload = UiTextureUpload::OutOfSync { texture_id };
        }
    }
}

/// Куда ложится одна дельта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeltaPlacement {
    /// Целая текстура.
    WholeTexture,
    /// Правка в границах известной текстуры.
    Patch { offset: [u32; 2] },
    /// Правка текстуры, которой у художника нет.
    UnknownTexture,
    /// Правка выходит за границы текстуры.
    PatchOutOfBounds,
}

/// Чистое правило «куда ложится дельта» (как `EguiCompositor::can_apply_image_delta` у wgpu).
pub(super) fn place_image_delta(
    existing_size: Option<[u32; 2]>,
    delta_pos: Option<[usize; 2]>,
    patch_size: [usize; 2],
) -> DeltaPlacement {
    let Some([x, y]) = delta_pos else {
        return DeltaPlacement::WholeTexture;
    };
    let Some([texture_width, texture_height]) = existing_size else {
        return DeltaPlacement::UnknownTexture;
    };
    let fits = |offset: usize, extent: usize, limit: u32| {
        offset
            .checked_add(extent)
            .and_then(|end| u32::try_from(end).ok())
            .is_some_and(|end| end <= limit)
    };
    if fits(x, patch_size[0], texture_width) && fits(y, patch_size[1], texture_height) {
        // Обе координаты ≤ размера текстуры (u32), поэтому приведение без потерь.
        DeltaPlacement::Patch {
            offset: [x as u32, y as u32],
        }
    } else {
        DeltaPlacement::PatchOutOfBounds
    }
}

/// Одна GL-текстура egui.
#[derive(Debug)]
struct UiTexture {
    name: glow::NativeTexture,
    size: [u32; 2],
}

/// Ограничения драйвера для загрузки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UploadLimits {
    pub(super) max_texture_side: u32,
    pub(super) unpack_row_length: UnpackRowLength,
}

/// Кэш GL-текстур egui по `TextureId`.
#[derive(Debug, Default)]
pub(super) struct UiTextureStore {
    textures: HashMap<TextureId, UiTexture>,
}

impl UiTextureStore {
    /// GL-имя текстуры для рисования.
    pub(super) fn gl_texture(&self, id: TextureId) -> Option<glow::NativeTexture> {
        self.textures.get(&id).map(|texture| texture.name)
    }

    /// Число живых текстур.
    pub(super) fn len(&self) -> usize {
        self.textures.len()
    }

    /// Применить `set` кадра. Ошибка GL (отказ создать объект, несовпадение длины пикселей) —
    /// прерывает применение: состояние текстур после неё неизвестно, вызывающий отдаёт её наружу.
    pub(super) fn apply_set<'delta>(
        &mut self,
        gl: DrawGl<'_>,
        set: impl IntoIterator<Item = (&'delta TextureId, &'delta [ImageDelta])>,
        limits: UploadLimits,
    ) -> Result<TextureApplyReport, DrawGlError> {
        let mut report = TextureApplyReport::empty();
        gl.set_tight_rgba_unpack(limits.unpack_row_length);
        for (&texture_id, image_deltas) in set {
            for image_delta in image_deltas {
                self.apply_one(gl, texture_id, image_delta, limits, &mut report)?;
            }
        }
        // Не оставлять текстуру egui привязанной: следующий слой (видео) начинает с чистого блока.
        gl.bind_texture_unit0(None);
        Ok(report)
    }

    fn apply_one(
        &mut self,
        gl: DrawGl<'_>,
        texture_id: TextureId,
        image_delta: &ImageDelta,
        limits: UploadLimits,
        report: &mut TextureApplyReport,
    ) -> Result<(), DrawGlError> {
        let ImageData::Color(image) = &image_delta.image;
        let existing_size = self.textures.get(&texture_id).map(|texture| texture.size);
        match place_image_delta(existing_size, image_delta.pos, image.size) {
            DeltaPlacement::UnknownTexture | DeltaPlacement::PatchOutOfBounds => {
                report.note_out_of_sync(texture_id);
                Ok(())
            }
            DeltaPlacement::Patch { offset } => {
                let Some(texture) = self.textures.get(&texture_id) else {
                    report.note_out_of_sync(texture_id);
                    return Ok(());
                };
                let patch_size = texel_size(image.size)?;
                gl.bind_texture_unit0(Some(texture.name));
                let (sampling, npot_fallback) = sampling_for(image_delta.options, texture.size);
                report.npot_wrap_fallbacks += u32::from(npot_fallback);
                gl.set_bound_texture_sampling(sampling);
                gl.upload_bound_texture_patch_rgba8(
                    offset,
                    patch_size,
                    bytemuck::cast_slice(&image.pixels),
                )?;
                report.patch_uploads += 1;
                Ok(())
            }
            DeltaPlacement::WholeTexture => {
                let size = texel_size(image.size)?;
                if size[0] > limits.max_texture_side || size[1] > limits.max_texture_side {
                    log::warn!(
                        "egui painter: texture {texture_id:?} {}x{} exceeds GL_MAX_TEXTURE_SIZE \
                         {}; skipped",
                        size[0],
                        size[1],
                        limits.max_texture_side
                    );
                    // Старое содержимое под этим id больше не то, что ждёт egui: убрать.
                    self.free(gl, texture_id);
                    report.oversized_skipped += 1;
                    return Ok(());
                }
                let name = match self.textures.get(&texture_id) {
                    Some(texture) => texture.name,
                    None => gl.create_texture()?,
                };
                // Запись в кэш до загрузки: если загрузка откажет, объект не потеряется и будет
                // удалён вместе с остальными.
                self.textures.insert(texture_id, UiTexture { name, size });
                gl.bind_texture_unit0(Some(name));
                let (sampling, npot_fallback) = sampling_for(image_delta.options, size);
                report.npot_wrap_fallbacks += u32::from(npot_fallback);
                gl.set_bound_texture_sampling(sampling);
                gl.upload_bound_texture_rgba8(size, bytemuck::cast_slice(&image.pixels))?;
                report.full_uploads += 1;
                Ok(())
            }
        }
    }

    /// Удалить текстуру. Неизвестный id — no-op (как `egui_wgpu::Renderer::free_texture`).
    pub(super) fn free(&mut self, gl: DrawGl<'_>, texture_id: TextureId) -> bool {
        match self.textures.remove(&texture_id) {
            Some(texture) => {
                gl.delete_texture(texture.name);
                true
            }
            None => false,
        }
    }

    /// Удалить все текстуры (разрушение художника).
    pub(super) fn delete_all(&mut self, gl: DrawGl<'_>) {
        for (_, texture) in self.textures.drain() {
            gl.delete_texture(texture.name);
        }
    }

    /// Забыть все имена без вызовов GL (контекст потерян: объекты умерли вместе с ним).
    pub(super) fn forget_all(&mut self) {
        self.textures.clear();
    }
}

/// Размер egui (`usize`) → размер GL (`u32`).
fn texel_size(size: [usize; 2]) -> Result<[u32; 2], DrawGlError> {
    let side = |value: usize| {
        u32::try_from(value).map_err(|_| DrawGlError::IntegerOverflow {
            what: "texture side",
        })
    };
    Ok([side(size[0])?, side(size[1])?])
}

/// Параметры выборки GL из `TextureOptions` egui. Второй элемент — был ли фолбэк повтора на
/// `CLAMP_TO_EDGE` для текстуры не степени двойки.
pub(super) fn sampling_for(options: TextureOptions, size: [u32; 2]) -> (TextureSampling, bool) {
    let filter = |filter: TextureFilter| match filter {
        TextureFilter::Nearest => glow::NEAREST,
        TextureFilter::Linear => glow::LINEAR,
    };
    let power_of_two = size[0].is_power_of_two() && size[1].is_power_of_two();
    let (wrap, npot_fallback) = match options.wrap_mode {
        TextureWrapMode::ClampToEdge => (glow::CLAMP_TO_EDGE, false),
        TextureWrapMode::Repeat if power_of_two => (glow::REPEAT, false),
        TextureWrapMode::MirroredRepeat if power_of_two => (glow::MIRRORED_REPEAT, false),
        TextureWrapMode::Repeat | TextureWrapMode::MirroredRepeat => {
            log::warn!(
                "egui painter: {:?} on a {}x{} (non power of two) texture is not supported by \
                 GLES 2.0; using CLAMP_TO_EDGE",
                options.wrap_mode,
                size[0],
                size[1]
            );
            (glow::CLAMP_TO_EDGE, true)
        }
    };
    (
        TextureSampling {
            minification: filter(options.minification),
            magnification: filter(options.magnification),
            wrap,
        },
        npot_fallback,
    )
}

#[cfg(test)]
mod tests {
    use epaint::textures::{TextureOptions, TextureWrapMode};

    use super::{DeltaPlacement, place_image_delta, sampling_for};

    #[test]
    fn delta_placement_matches_wgpu_rules() {
        assert_eq!(
            place_image_delta(None, None, [4, 4]),
            DeltaPlacement::WholeTexture
        );
        assert_eq!(
            place_image_delta(None, Some([1, 1]), [2, 2]),
            DeltaPlacement::UnknownTexture
        );
        assert_eq!(
            place_image_delta(Some([4, 4]), Some([2, 2]), [2, 2]),
            DeltaPlacement::Patch { offset: [2, 2] }
        );
        assert_eq!(
            place_image_delta(Some([4, 4]), Some([3, 0]), [2, 1]),
            DeltaPlacement::PatchOutOfBounds
        );
        assert_eq!(
            place_image_delta(Some([4, 4]), Some([usize::MAX, 0]), [2, 1]),
            DeltaPlacement::PatchOutOfBounds
        );
    }

    #[test]
    fn repeat_on_npot_texture_falls_back_to_clamp() {
        let repeat = TextureOptions {
            wrap_mode: TextureWrapMode::Repeat,
            ..TextureOptions::LINEAR
        };
        let (pot, pot_fallback) = sampling_for(repeat, [64, 32]);
        assert_eq!((pot.wrap, pot_fallback), (glow::REPEAT, false));
        let (npot, npot_fallback) = sampling_for(repeat, [60, 32]);
        assert_eq!((npot.wrap, npot_fallback), (glow::CLAMP_TO_EDGE, true));
        let (linear, _) = sampling_for(TextureOptions::NEAREST, [3, 3]);
        assert_eq!(
            (linear.minification, linear.magnification, linear.wrap),
            (glow::NEAREST, glow::NEAREST, glow::CLAMP_TO_EDGE)
        );
    }
}
