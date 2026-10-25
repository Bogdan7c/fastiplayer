//! Атлас одного вида пикселей: текстура, буфер вершин, какие картинки сейчас в атласе, счётчики.
//!
//! Порт `BitmapLayer` wgpu-прохода (`render-wgpu-shell/src/subtitle_overlay/layer.rs`) с теми же
//! правилами и счётчиками:
//! - те же пиксели, что уже в атласе (по [`OverlayBitmap::shows_same_pixels_as`] каждой картинки),
//!   — загрузки нет, переписываются только вершины (положение и цвет могли измениться);
//! - иначе упаковка с текущей стороны (атлас не уменьшается), рост ×2 до предела, загрузка всех
//!   картинок вида;
//! - не влезло даже в предельный атлас — вид в этом кадре не рисуется, картинки считаются
//!   пропущенными, атлас забывает содержимое.

use crate::gl_calls::VideoGl;
use crate::gl_calls::overlay::AtlasTextureFormat;
use crate::planes::HostRows;

use super::api::{OverlayAtlasOutcome, OverlayAtlasStats, OverlayPrepareError};
use super::atlas::{AtlasSlot, MaskExtent, pack_masks};
use super::upload_calls::{RowStepSupport, atlas_upload_calls};
use super::vertices::{BitmapQuad, VERTEX_LAYOUT, VERTICES_PER_BITMAP, append_bitmap_quad};
use super::{OverlayBitmap, OverlayPixels};

/// Что получает атлас от раскладки кадра.
pub(super) struct LayerPrepareInput<'list, 'bitmap, B> {
    /// Картинки вида, влезающие в предел, в порядке списка.
    pub(super) bitmaps: &'list [&'bitmap B],
    /// Сколько картинок вида отброшено раскладкой (сторона больше предела).
    pub(super) dropped_by_size: usize,
    /// Предел стороны атласа.
    pub(super) max_side: u32,
    /// Чем задаётся шаг строк при загрузке.
    pub(super) row_step: RowStepSupport,
}

/// GL-ресурсы для рисования вида.
#[derive(Debug, Clone, Copy)]
pub(super) struct LayerDrawResources {
    /// Атлас.
    pub(super) texture: glow::NativeTexture,
    /// Буфер вершин.
    pub(super) vertex_buffer: glow::NativeBuffer,
    /// Вершин в буфере.
    pub(super) vertex_count: usize,
}

/// Картинка с местом в атласе.
struct PlacedBitmap<'a, B> {
    /// Номер картинки среди картинок вида (для ошибки).
    index: usize,
    /// Картинка.
    bitmap: &'a B,
    /// Где её пиксели в атласе.
    slot: AtlasSlot,
}

/// Текстура атласа и её сторона.
#[derive(Debug, Clone, Copy)]
struct AtlasTexture {
    texture: glow::NativeTexture,
    side: u32,
}

/// Атлас одного вида пикселей.
pub(super) struct AtlasLayer<B> {
    format: AtlasTextureFormat,
    /// Создаётся при первой картинке вида (у RGBA — лениво, как второй слой wgpu).
    atlas: Option<AtlasTexture>,
    vertex_buffer: Option<glow::NativeBuffer>,
    vertex_count: usize,
    /// Байты вершин, переиспользуемые между подготовками (без аллокации на каждую смену).
    vertex_bytes: Vec<u8>,
    /// Клоны картинок, чьи пиксели сейчас в атласе (клон держит буфер пикселей живым).
    resident: Vec<B>,
    /// Слоты `resident` в атласе.
    resident_slots: Vec<AtlasSlot>,
    stats: OverlayAtlasStats,
}

impl<B: OverlayBitmap> AtlasLayer<B> {
    /// Пустой атлас вида без GL-объектов.
    pub(super) const fn new(format: AtlasTextureFormat) -> Self {
        Self {
            format,
            atlas: None,
            vertex_buffer: None,
            vertex_count: 0,
            vertex_bytes: Vec::new(),
            resident: Vec::new(),
            resident_slots: Vec::new(),
            stats: OverlayAtlasStats {
                atlas_uploads: 0,
                bitmap_uploads: 0,
                atlas_recreations: 0,
                skipped_prepares: 0,
                skipped_bitmaps: 0,
                upload_calls: 0,
                atlas_side: 0,
            },
        }
    }

    /// Счётчики.
    pub(super) fn stats(&self) -> OverlayAtlasStats {
        OverlayAtlasStats {
            atlas_side: self.atlas.map_or(0, |atlas| atlas.side),
            ..self.stats
        }
    }

    /// Подготовить вид к рисованию: загрузить атлас, если пиксели сменились, и переписать вершины.
    ///
    /// После ошибки GL вид ничего не рисует и не помнит содержимое атласа (следующая подготовка
    /// загрузит всё заново).
    pub(super) fn prepare(
        &mut self,
        gl: VideoGl<'_>,
        input: &LayerPrepareInput<'_, '_, B>,
    ) -> Result<OverlayAtlasOutcome, OverlayPrepareError> {
        let mut lost = input.dropped_by_size;
        let prepared = self.prepare_kept(gl, input);
        let outcome = match prepared {
            Ok(outcome) => outcome,
            Err(error) => {
                self.forget_contents();
                return Err(error);
            }
        };
        if let OverlayAtlasOutcome::DoesNotFit { bitmaps } = outcome {
            lost += bitmaps;
        }
        if lost > 0 {
            self.stats.skipped_prepares += 1;
            self.stats.skipped_bitmaps += lost as u64;
        }
        Ok(outcome)
    }

    fn prepare_kept(
        &mut self,
        gl: VideoGl<'_>,
        input: &LayerPrepareInput<'_, '_, B>,
    ) -> Result<OverlayAtlasOutcome, OverlayPrepareError> {
        let bitmaps = input.bitmaps;
        if bitmaps.is_empty() {
            self.forget_contents();
            return Ok(OverlayAtlasOutcome::NotNeeded);
        }
        if self.is_resident(bitmaps) {
            self.write_vertices(gl, bitmaps)?;
            return Ok(OverlayAtlasOutcome::AlreadyResident {
                bitmaps: bitmaps.len(),
            });
        }
        let extents: Vec<MaskExtent> = bitmaps
            .iter()
            .map(|bitmap| {
                let rect = bitmap.rect();
                MaskExtent {
                    width: rect.width,
                    height: rect.height,
                }
            })
            .collect();
        let current_side = self.atlas.map_or(0, |atlas| atlas.side);
        let Ok(layout) = pack_masks(&extents, current_side, input.max_side) else {
            self.forget_contents();
            log::warn!(
                "gles2-video: {} overlay bitmap(s) do not fit a {}x{} atlas, skipped",
                bitmaps.len(),
                input.max_side,
                input.max_side
            );
            return Ok(OverlayAtlasOutcome::DoesNotFit {
                bitmaps: bitmaps.len(),
            });
        };
        let texture = self.ensure_atlas(gl, layout.side)?;
        // Прежнее содержимое больше не годится: запись идёт поверх, сверка «то же» — только после
        // успешной загрузки всего набора.
        self.resident.clear();
        self.resident_slots.clear();
        for (index, (bitmap, slot)) in bitmaps.iter().zip(&layout.slots).enumerate() {
            let placed = PlacedBitmap {
                index,
                bitmap: *bitmap,
                slot: *slot,
            };
            self.upload_bitmap(gl, texture, &placed, input.row_step)?;
        }
        self.stats.bitmap_uploads += bitmaps.len() as u64;
        self.stats.atlas_uploads += 1;
        self.resident = bitmaps.iter().map(|bitmap| (*bitmap).clone()).collect();
        self.resident_slots = layout.slots;
        self.write_vertices(gl, bitmaps)?;
        Ok(OverlayAtlasOutcome::Uploaded {
            bitmaps: bitmaps.len(),
        })
    }

    /// Те же пиксели в том же порядке уже лежат в атласе.
    fn is_resident(&self, bitmaps: &[&B]) -> bool {
        self.atlas.is_some()
            && self.resident.len() == bitmaps.len()
            && self
                .resident
                .iter()
                .zip(bitmaps)
                .all(|(resident, candidate)| resident.shows_same_pixels_as(candidate))
    }

    /// Текстура атласа стороной `side`: создаётся при первом использовании, при росте выделяется
    /// заново (содержимое теряется — его тут же перезаписывают).
    fn ensure_atlas(
        &mut self,
        gl: VideoGl<'_>,
        side: u32,
    ) -> Result<glow::NativeTexture, OverlayPrepareError> {
        if let Some(atlas) = self.atlas
            && atlas.side == side
        {
            return Ok(atlas.texture);
        }
        let texture = match self.atlas.take() {
            Some(atlas) => atlas.texture,
            None => gl.create_texture().map_err(OverlayPrepareError::Gl)?,
        };
        if let Err(error) = gl.allocate_atlas_texture(texture, self.format, side) {
            gl.delete_texture(texture);
            return Err(OverlayPrepareError::Gl(error));
        }
        self.stats.atlas_recreations += 1;
        self.atlas = Some(AtlasTexture { texture, side });
        Ok(texture)
    }

    /// Загрузить пиксели одной картинки в её слот.
    fn upload_bitmap(
        &mut self,
        gl: VideoGl<'_>,
        texture: glow::NativeTexture,
        placed: &PlacedBitmap<'_, B>,
        row_step: RowStepSupport,
    ) -> Result<(), OverlayPrepareError> {
        let pixels = placed.bitmap.pixels();
        let rows = checked_rows(placed.index, pixels, placed.bitmap)?;
        let calls = atlas_upload_calls(&rows, self.format.bytes_per_texel(), placed.slot, row_step)
            .ok_or(OverlayPrepareError::Gl(
                crate::gl_calls::VideoGlError::IntegerOverflow {
                    what: "overlay upload plan",
                },
            ))?;
        for call in &calls {
            gl.upload_atlas_rows(texture, self.format, call, pixels.bytes)
                .map_err(OverlayPrepareError::Gl)?;
            self.stats.upload_calls += 1;
        }
        Ok(())
    }

    /// Переписать вершины вида по текущим картинкам и их слотам в атласе.
    fn write_vertices(
        &mut self,
        gl: VideoGl<'_>,
        bitmaps: &[&B],
    ) -> Result<(), OverlayPrepareError> {
        let Some(atlas) = self.atlas else {
            return Ok(());
        };
        let buffer = match self.vertex_buffer {
            Some(buffer) => buffer,
            None => {
                let buffer = gl.create_buffer().map_err(OverlayPrepareError::Gl)?;
                self.vertex_buffer = Some(buffer);
                buffer
            }
        };
        self.vertex_bytes.clear();
        self.vertex_bytes
            .reserve(bitmaps.len() * VERTICES_PER_BITMAP * VERTEX_LAYOUT.stride);
        for (bitmap, slot) in bitmaps.iter().zip(&self.resident_slots) {
            let quad = BitmapQuad {
                rect: bitmap.rect(),
                slot: *slot,
                tint: bitmap.tint(),
                group: bitmap.placement_group(),
            };
            append_bitmap_quad(&mut self.vertex_bytes, &quad, atlas.side);
        }
        gl.upload_dynamic_vertices(buffer, &self.vertex_bytes);
        self.vertex_count = bitmaps.len() * VERTICES_PER_BITMAP;
        Ok(())
    }

    /// Ресурсы для рисования; `None` — вид пуст или GL-объектов нет.
    pub(super) fn draw_resources(&self) -> Option<LayerDrawResources> {
        if self.vertex_count == 0 {
            return None;
        }
        Some(LayerDrawResources {
            texture: self.atlas?.texture,
            vertex_buffer: self.vertex_buffer?,
            vertex_count: self.vertex_count,
        })
    }

    /// Забыть содержимое атласа (текстура остаётся для следующей загрузки).
    pub(super) fn forget_contents(&mut self) {
        self.resident.clear();
        self.resident_slots.clear();
        self.vertex_count = 0;
    }

    /// Удалить GL-объекты.
    pub(super) fn delete(&mut self, gl: VideoGl<'_>) {
        self.forget_contents();
        if let Some(atlas) = self.atlas.take() {
            gl.delete_texture(atlas.texture);
        }
        if let Some(buffer) = self.vertex_buffer.take() {
            gl.delete_buffer(buffer);
        }
    }
}

impl<B> AtlasLayer<B> {
    /// Есть ли GL-объекты (для предупреждения при `Drop` без `destroy`).
    pub(super) const fn has_gl_objects(&self) -> bool {
        self.atlas.is_some() || self.vertex_buffer.is_some()
    }
}

/// Проверенные строки картинки: срез покрывает все видимые байты размера картинки.
pub(super) fn checked_rows<'a, B: OverlayBitmap>(
    index: usize,
    pixels: OverlayPixels<'a>,
    bitmap: &B,
) -> Result<HostRows<'a>, OverlayPrepareError> {
    let rect = bitmap.rect();
    let malformed = |source| OverlayPrepareError::MalformedBitmap { index, source };
    let row_bytes = usize::try_from(rect.width)
        .ok()
        .and_then(|width| width.checked_mul(pixels.format.bytes_per_pixel()))
        .ok_or(malformed(crate::planes::PlaneSourceError::SizeOverflow))?;
    HostRows::new(pixels.bytes, pixels.stride, row_bytes, rect.height).map_err(malformed)
}
