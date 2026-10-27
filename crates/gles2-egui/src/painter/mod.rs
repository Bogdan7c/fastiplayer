//! Художник egui на OpenGL ES 2.0 ([`EguiPainter`]).
//!
//! Паритет с egui-wgpu 0.36 — источник истины для внешнего вида (решение владельца 7):
//! premultiplied alpha, смешивание в гамма-пространстве (`ONE, ONE_MINUS_SRC_ALPHA` для цвета,
//! `ONE_MINUS_DST_ALPHA, ONE` для альфы), текстуры RGBA8 не sRGB, фильтры из `TextureOptions`,
//! scissor по правилу egui-wgpu, дизеринг как у egui-wgpu (решение 29).
//!
//! Стадии кадра повторяют `EguiCompositor` у wgpu, чтобы адаптер `RenderShell` (сессия 09)
//! переводил контракт один к одному:
//! 1. [`EguiPainter::apply_textures`] — `set` первым шагом, даже если кадр потом отклонят;
//! 2. [`EguiPainter::retained_availability`] — можно ли повторить прошлый UI на этом экране;
//! 3. [`EguiPainter::prepare_fresh`] — новый кадр в буферы GPU, он же становится удержанным;
//! 4. [`EguiPainter::paint_retained`] — нарисовать удержанный кадр (свежий или повтор);
//! 5. [`EguiPainter::free_textures`] — `free` после показа (или сразу у отклонённого кадра).
//!
//! Повтор UI (`ReusePrevious`, cpu-parity 04) — только проигрывание списка draw-call-ов из уже
//! загруженных буферов: ни вершин, ни индексов, ни текстур. Это закрепляют счётчики
//! [`EguiPainterStats`].

mod drawing;
mod program;
mod scissor;
mod textures;
mod ui_damage;
mod upload;

use epaint::textures::TexturesDelta;
use epaint::{ClippedPrimitive, TextureId};
use thiserror::Error;

use crate::context::{ContextId, GlAccessError, Gles2Context};
use crate::draw_gl::{BufferTarget, DrawGl, DrawGlError, IndexType, UnpackRowLength};
use crate::frame::ActiveFrame;
use crate::shader::ShaderError;

pub use self::program::EffectiveDithering;
pub use self::textures::{TextureApplyReport, UiTextureUpload};
pub use self::upload::UiPrepareWork;

use self::drawing::create_buffer_objects;
use self::program::PainterProgram;
use self::scissor::paint_scissor_limit;
use self::textures::{UiTextureStore, UploadLimits};
use self::ui_damage::TextureChange;
pub(crate) use self::ui_damage::UiFootprint;
use self::upload::FrameStaging;

/// Размер экрана кадра egui (аналог `egui_wgpu::ScreenDescriptor`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiScreen {
    /// Размер цели в физических пикселях.
    pub size_in_pixels: [u32; 2],
    /// Пикселей на точку egui.
    pub pixels_per_point: f32,
}

impl UiScreen {
    /// Размер экрана в точках — то, что ждёт вершинный шейдер.
    fn size_in_points(self) -> [f32; 2] {
        [
            self.size_in_pixels[0] as f32 / self.pixels_per_point,
            self.size_in_pixels[1] as f32 / self.pixels_per_point,
        ]
    }

    /// Тот же экран, что у удержанного кадра: размер и `pixels_per_point` побитово (как
    /// `EguiCompositor::retained_frame_availability` у wgpu).
    fn same_as(self, other: Self) -> bool {
        self.size_in_pixels == other.size_in_pixels
            && self.pixels_per_point.to_bits() == other.pixels_per_point.to_bits()
    }
}

/// Дизеринг UI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum UiDithering {
    /// Как egui-wgpu по умолчанию (`RendererOptions::dithering = true`). Требует `highp` во
    /// фрагментном шейдере; без него выключается сам ([`EffectiveDithering`]).
    #[default]
    MatchEguiWgpu,
    /// Без шума.
    Disabled,
}

/// Ширина индексов мешей.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum IndexWidthPreference {
    /// `u32` при `GL_OES_element_index_uint`, иначе батчи `u16`.
    #[default]
    BestAvailable,
    /// Всегда батчи `u16` (путь драйверов без расширения; обход драйверов с ошибками в нём).
    U16Batches,
}

/// Объект массива вершин (VAO).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum VertexArrayPreference {
    /// VAO при `GL_OES_vertex_array_object`, иначе явная настройка атрибутов.
    #[default]
    UseWhenAvailable,
    /// Всегда явная настройка атрибутов (путь драйверов без расширения).
    ExplicitAttributes,
}

/// Настройки художника.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EguiPainterOptions {
    /// Дизеринг.
    pub dithering: UiDithering,
    /// Ширина индексов.
    pub index_width: IndexWidthPreference,
    /// VAO.
    pub vertex_array: VertexArrayPreference,
}

/// Почему нельзя повторить прошлый UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetainedUiUnavailable {
    /// Удержанного кадра нет (художник новый, или кадр забыт).
    NothingRetained,
    /// Другой размер или `pixels_per_point`: позиции и scissor удержанного кадра неверны.
    ScreenMismatch,
}

/// Ошибки художника.
#[derive(Debug, Error)]
pub enum PainterError {
    /// Художник создан в другом контексте: его GL-имена здесь ничего не значат.
    #[error("egui painter belongs to another GLES2 context")]
    ForeignContext,
    /// Контекст недоступен (потерян, окно отпущено без surfaceless-контекста, отказ make current).
    #[error("GLES2 context unavailable: {0}")]
    GlAccess(#[source] GlAccessError),
    /// Программа не собралась.
    #[error("egui painter shader: {0}")]
    Shader(#[source] ShaderError),
    /// В слинкованной программе нет нужного входа (драйвер выкинул или переименовал).
    #[error("egui painter shader has no input `{name}`")]
    MissingShaderInput {
        /// Имя атрибута или uniform.
        name: &'static str,
    },
    /// Отказ GL-операции (создание объекта, размеры).
    #[error("egui painter GL operation: {0}")]
    Gl(#[source] DrawGlError),
    /// Нарисовать нечего: удержанного кадра нет или экран другой. Вызывающий обязан был
    /// проверить [`EguiPainter::retained_availability`].
    #[error("no retained egui frame for this screen: {0:?}")]
    RetainedUnavailable(RetainedUiUnavailable),
}

/// Итог `free` кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFreeOutcome {
    /// Освобождено сейчас (неизвестные id — no-op, как у egui-wgpu).
    Applied {
        /// Сколько текстур реально удалено.
        deleted: u32,
    },
    /// GL сейчас недоступен (окно отпущено без surfaceless-контекста): освобождение отложено
    /// до следующего доступа художника к GL. Потерянный контекст — тоже сюда: объекты умерли.
    Deferred {
        /// Сколько id ждут.
        pending: u32,
    },
}

/// Работа одного рисования удержанного кадра.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UiPaintWork {
    /// `glDrawElements`.
    pub draw_calls: u32,
    /// Привязок текстур (подряд идущие меши одной текстуры не перепривязываются).
    pub texture_binds: u32,
    /// Мешей пропущено: их текстуры нет (как `log::warn!("Missing texture")` у egui-wgpu).
    pub skipped_missing_texture: u32,
    /// Команд пропущено: целиком вне области частичной перерисовки кадра.
    pub skipped_outside_repaint: u32,
}

/// Счётчики художника за всё время (оптимизации доказываются ими, а не «функция вызвалась»).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EguiPainterStats {
    /// Свежих кадров, собранных в буферы.
    pub fresh_prepares: u64,
    /// Загрузок вершинного + индексного буфера (по одной на свежий кадр).
    pub buffer_streams: u64,
    /// Байт вершин и индексов, ушедших в GPU.
    pub streamed_bytes: u64,
    /// Полных загрузок текстур.
    pub texture_full_uploads: u64,
    /// Частичных правок текстур.
    pub texture_patch_uploads: u64,
    /// Рисований удержанного кадра.
    pub paints: u64,
    /// `glDrawElements` за всё время.
    pub draw_calls: u64,
}

/// Художник egui. Владеет GL-объектами в одном контексте ([`ContextId`]).
///
/// Разрушение — [`Self::destroy`] при живом контексте. Drop без него только забывает имена:
/// объекты умрут вместе с контекстом (так и есть после потери контекста), но если контекст
/// живёт дальше — это утечка видеопамяти, о ней пишется предупреждение.
#[derive(Debug)]
pub struct EguiPainter {
    owner: ContextId,
    program: PainterProgram,
    vertex_buffer: glow::NativeBuffer,
    index_buffer: glow::NativeBuffer,
    /// VAO при `GL_OES_vertex_array_object`; без него атрибуты описываются на каждом рисовании.
    vertex_array: Option<glow::NativeVertexArray>,
    textures: UiTextureStore,
    staging: FrameStaging,
    /// Экран удержанного кадра; `None` — удержанного нет.
    retained_screen: Option<UiScreen>,
    limits: UploadLimits,
    /// `free`, которые не удалось выполнить без GL.
    deferred_frees: Vec<TextureId>,
    /// Правки текстур, применённые с прошлой сборки кадра: следующая сборка повредит
    /// треугольники затронутых текстур (см. `ui_damage`).
    pending_texture_changes: Vec<(TextureId, TextureChange)>,
    stats: EguiPainterStats,
    destroyed: bool,
}

impl EguiPainter {
    /// Собирает программу, буферы и (если есть расширение) VAO.
    pub fn new(context: &Gles2Context, options: EguiPainterOptions) -> Result<Self, PainterError> {
        let capabilities = context.capabilities();
        let conveniences = capabilities.tiers().conveniences;
        let index_type = match options.index_width {
            IndexWidthPreference::BestAvailable if conveniences.element_index_uint => {
                IndexType::U32
            }
            IndexWidthPreference::BestAvailable | IndexWidthPreference::U16Batches => {
                IndexType::U16
            }
        };
        let use_vertex_array = conveniences.vertex_array_object
            && options.vertex_array == VertexArrayPreference::UseWhenAvailable;
        let limits = UploadLimits {
            max_texture_side: capabilities.max_texture_size(),
            unpack_row_length: if conveniences.unpack_subimage {
                UnpackRowLength::ResetSupported
            } else {
                UnpackRowLength::NotSupported
            },
        };
        let program = PainterProgram::build(context, options.dithering)?;
        let gl = DrawGl::for_context(context).map_err(PainterError::GlAccess)?;
        let objects = create_buffer_objects(gl, use_vertex_array);
        let (vertex_buffer, index_buffer, vertex_array) = match objects {
            Ok(objects) => objects,
            Err(error) => {
                gl.use_program(None);
                if let Err(delete_error) = context.delete_program(program.program) {
                    log::warn!("egui painter: failed to delete program: {delete_error}");
                }
                return Err(PainterError::Gl(error));
            }
        };
        let painter = Self {
            owner: context.id(),
            program,
            vertex_buffer,
            index_buffer,
            vertex_array,
            textures: UiTextureStore::default(),
            staging: FrameStaging::new(index_type),
            retained_screen: None,
            limits,
            deferred_frees: Vec::new(),
            pending_texture_changes: Vec::new(),
            stats: EguiPainterStats::default(),
            destroyed: false,
        };
        if let Some(vertex_array) = painter.vertex_array {
            // VAO запоминает привязку буфера индексов и описание атрибутов один раз.
            gl.bind_vertex_array(Some(vertex_array));
            painter.bind_frame_vertex_layout(gl, 0);
            gl.bind_vertex_array(None);
        }
        Ok(painter)
    }

    /// Отпечаток удержанного UI-кадра: основа повреждения для частичного показа. Сборщик
    /// кадра сравнивает его с отпечатком прошлого показанного кадра.
    pub(crate) fn retained_footprint(&self) -> &UiFootprint {
        self.staging.footprint()
    }

    /// Сторона текстуры, которую стоит передавать egui (`RawInput::max_texture_side`).
    pub fn max_texture_side(&self) -> usize {
        self.limits.max_texture_side as usize
    }

    /// Что реально стоит с дизерингом.
    pub fn dithering(&self) -> EffectiveDithering {
        self.program.dithering
    }

    /// Тип индексов (`u32` с `GL_OES_element_index_uint`, иначе батчи `u16`).
    pub fn uses_u32_indices(&self) -> bool {
        self.staging.index_type() == IndexType::U32
    }

    /// Стоит ли VAO (иначе атрибуты описываются на каждом рисовании).
    pub fn uses_vertex_array(&self) -> bool {
        self.vertex_array.is_some()
    }

    /// Счётчики за всё время.
    pub fn stats(&self) -> EguiPainterStats {
        self.stats
    }

    /// Живых текстур egui.
    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }

    /// Шаг 1 кадра: применить `set` (и отложенные `free` прошлых кадров). Вне кадра: контекст
    /// делается текущим здесь, поэтому изменения применяются, даже если кадр затем не начнётся.
    pub fn apply_textures(
        &mut self,
        context: &Gles2Context,
        delta: &TexturesDelta,
    ) -> Result<TextureApplyReport, PainterError> {
        self.ensure_owner(context.id())?;
        let gl = DrawGl::for_context(context).map_err(PainterError::GlAccess)?;
        self.apply_deferred_frees(gl);
        let set = delta
            .set
            .iter()
            .map(|(texture_id, image_deltas)| (texture_id, image_deltas.as_slice()));
        let report = self
            .textures
            .apply_set(gl, set, self.limits)
            .map_err(PainterError::Gl)?;
        self.stats.texture_full_uploads += u64::from(report.full_uploads);
        self.stats.texture_patch_uploads += u64::from(report.patch_uploads);
        for (texture_id, image_deltas) in &delta.set {
            for image_delta in image_deltas {
                let change = if image_delta.pos.is_some() {
                    TextureChange::Patched
                } else {
                    TextureChange::Replaced
                };
                self.pending_texture_changes.push((*texture_id, change));
            }
        }
        Ok(report)
    }

    /// Шаг 2: можно ли повторить удержанный кадр на этом экране.
    pub fn retained_availability(&self, screen: UiScreen) -> Result<(), RetainedUiUnavailable> {
        match self.retained_screen {
            None => Err(RetainedUiUnavailable::NothingRetained),
            Some(retained) if retained.same_as(screen) => Ok(()),
            Some(_) => Err(RetainedUiUnavailable::ScreenMismatch),
        }
    }

    /// Шаг 3: новый кадр egui в буферы GPU. Он же становится удержанным для повтора.
    pub fn prepare_fresh(
        &mut self,
        frame: &ActiveFrame<'_>,
        screen: UiScreen,
        primitives: Vec<ClippedPrimitive>,
    ) -> Result<UiPrepareWork, PainterError> {
        self.ensure_owner(frame.context_id())?;
        let gl = DrawGl::for_active_frame(frame);
        // До сборки: если загрузка откажет, старый удержанный кадр уже недействителен (буферы
        // наполовину перезаписаны), повтор должен честно отказать.
        self.retained_screen = None;
        let texture_changes = std::mem::take(&mut self.pending_texture_changes);
        let work = self.staging.build(
            primitives,
            screen.pixels_per_point,
            screen.size_in_pixels,
            &texture_changes,
        );
        self.stream_staging(gl)?;
        self.retained_screen = Some(screen);
        self.stats.fresh_prepares += 1;
        self.stats.buffer_streams += 1;
        self.stats.streamed_bytes += (work.vertex_bytes + work.index_bytes) as u64;
        if work.skipped_paint_callbacks > 0 {
            log::warn!(
                "egui painter: {} paint callback(s) skipped: GLES2 painter has no callbacks",
                work.skipped_paint_callbacks
            );
        }
        if work.skipped_invalid_meshes > 0 {
            log::warn!(
                "egui painter: {} invalid mesh(es) skipped (index out of vertex range)",
                work.skipped_invalid_meshes
            );
        }
        Ok(work)
    }

    /// Шаг 4: нарисовать удержанный кадр поверх того, что уже в цели (видео, оверлеи).
    pub fn paint_retained(
        &mut self,
        frame: &ActiveFrame<'_>,
        screen: UiScreen,
    ) -> Result<UiPaintWork, PainterError> {
        self.ensure_owner(frame.context_id())?;
        self.retained_availability(screen)
            .map_err(PainterError::RetainedUnavailable)?;
        let gl = DrawGl::for_active_frame(frame);
        let [width, height] = screen.size_in_pixels;
        let (Ok(width), Ok(height)) = (i32::try_from(width), i32::try_from(height)) else {
            return Err(PainterError::Gl(DrawGlError::IntegerOverflow {
                what: "screen size",
            }));
        };
        let limit = paint_scissor_limit(frame.repaint_area(), screen.size_in_pixels[1]);
        self.begin_paint_state(gl, screen, width, height);
        let work = self.draw_commands(gl, &limit);
        self.end_paint_state(gl);
        self.stats.paints += 1;
        self.stats.draw_calls += u64::from(work.draw_calls);
        Ok(work)
    }

    /// Шаг 5: `free` кадра и очистка `TexturesDelta` (egui 0.36 паникует в debug на непустой
    /// при drop). Без доступа к GL освобождение откладывается, а не теряется.
    pub fn free_textures(
        &mut self,
        context: &Gles2Context,
        mut delta: TexturesDelta,
    ) -> Result<TextureFreeOutcome, PainterError> {
        if let Err(error) = self.ensure_owner(context.id()) {
            // Id чужого контекста к нашим текстурам не относятся; дельту всё равно очистить.
            delta.clear();
            return Err(error);
        }
        self.deferred_frees.extend(delta.free.drain());
        delta.clear();
        match DrawGl::for_context(context) {
            Ok(gl) => {
                let deleted = self.apply_deferred_frees(gl);
                Ok(TextureFreeOutcome::Applied { deleted })
            }
            Err(error) => {
                log::debug!("egui painter: texture free deferred: {error}");
                Ok(TextureFreeOutcome::Deferred {
                    pending: u32::try_from(self.deferred_frees.len()).unwrap_or(u32::MAX),
                })
            }
        }
    }

    /// Забыть удержанный кадр (следующий повтор откажет `NothingRetained`).
    pub fn forget_retained(&mut self) {
        self.retained_screen = None;
        self.staging.clear_commands();
    }

    /// Удалить все GL-объекты. После потери контекста — только забыть имена (GL недоступен,
    /// объекты умерли вместе с контекстом).
    pub fn destroy(mut self, context: &Gles2Context) -> Result<(), PainterError> {
        self.ensure_owner(context.id())?;
        self.destroyed = true;
        match DrawGl::for_context(context) {
            Ok(gl) => {
                self.textures.delete_all(gl);
                gl.delete_buffer(self.vertex_buffer);
                gl.delete_buffer(self.index_buffer);
                if let Some(vertex_array) = self.vertex_array {
                    gl.delete_vertex_array(vertex_array);
                }
                gl.use_program(None);
                context
                    .delete_program(self.program.program)
                    .map_err(PainterError::GlAccess)
            }
            Err(GlAccessError::ContextLost(_)) => {
                self.textures.forget_all();
                Ok(())
            }
            Err(error) => Err(PainterError::GlAccess(error)),
        }
    }

    fn ensure_owner(&self, context: ContextId) -> Result<(), PainterError> {
        if context == self.owner {
            Ok(())
        } else {
            Err(PainterError::ForeignContext)
        }
    }

    fn apply_deferred_frees(&mut self, gl: DrawGl<'_>) -> u32 {
        let mut deleted = 0;
        for texture_id in self.deferred_frees.drain(..) {
            deleted += u32::from(self.textures.free(gl, texture_id));
        }
        deleted
    }

    /// Загрузить собранный кадр с «осиротением» буферов.
    fn stream_staging(&self, gl: DrawGl<'_>) -> Result<(), PainterError> {
        // Буфер индексов привязывается к VAO художника, а не к чужому (видео-слой мог оставить
        // свой привязанным). Без расширения VAO функций `glBindVertexArrayOES` нет вовсе —
        // их нельзя звать даже для отвязки.
        if let Some(vertex_array) = self.vertex_array {
            gl.bind_vertex_array(Some(vertex_array));
        }
        gl.bind_array_buffer(Some(self.vertex_buffer));
        gl.bind_element_buffer(Some(self.index_buffer));
        let streamed = gl
            .stream_into_bound_buffer(BufferTarget::Vertices, self.staging.vertex_bytes())
            .and_then(|()| {
                gl.stream_into_bound_buffer(BufferTarget::Indices, self.staging.index_bytes())
            });
        if self.vertex_array.is_some() {
            gl.bind_vertex_array(None);
        }
        streamed.map_err(PainterError::Gl)
    }
}

impl Drop for EguiPainter {
    fn drop(&mut self) {
        if !self.destroyed {
            log::warn!(
                "egui painter dropped without destroy(): {} texture(s) and its buffers live until \
                 the GLES2 context is destroyed",
                self.textures.len()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::UiScreen;

    #[test]
    fn retained_screen_comparison_is_bitwise_like_wgpu() {
        let screen = UiScreen {
            size_in_pixels: [800, 600],
            pixels_per_point: 1.25,
        };
        assert!(screen.same_as(screen));
        assert!(!screen.same_as(UiScreen {
            size_in_pixels: [801, 600],
            ..screen
        }));
        assert!(!screen.same_as(UiScreen {
            pixels_per_point: 1.250_000_1,
            ..screen
        }));
        assert_eq!(
            [800.0 / 1.25, 600.0 / 1.25],
            screen.size_in_points(),
            "vertex shader works in points"
        );
    }
}
