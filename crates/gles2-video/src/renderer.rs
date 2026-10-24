//! [`VideoLayerRenderer`]: загрузка кадра в кольцо текстур и рисование видеослоя.
//!
//! Жизнь кадра у вызывающего:
//! 1. до кадра показа — [`VideoLayerRenderer::upload`] (одна копия драйвером, или повтор без
//!    загрузки, или `Busy`);
//! 2. внутри кадра показа — [`VideoLayerRenderer::draw`] (обычно из
//!    [`crate::VideoUnderlay`] — слоя `FrameLayers` сборщика кадра `gles2-egui`);
//! 3. после того как работа кадра отдана драйверу (после `FrameCompositor::render`) —
//!    [`VideoLayerRenderer::retire_submitted_work`] с fence этого кадра (`Gles2Context::insert_fence`).
//!    Один fence на кадр показа: он же охраняет и кадр декодера у вызывающего.
//!
//! Владение: рендер владеет программой, буфером вершин и текстурами кольца; байты кадра он
//! только читает во время `upload` и не хранит.

use std::sync::Arc;

use gles2_egui::{ActiveFrame, ContextId, Gles2Context, GpuFence};

use crate::draw_uniforms::{DrawUniformValues, PlaneSampling};
use crate::geometry::{PixelRect, to_gl_window_rect, uncovered_parts};
use crate::gl_calls::{PlaneTextureFormat, VideoGl};
use crate::layer_api::{
    PlaneFormatPreference, RowLengthPreference, UploadOutcome, UploadReport, VideoDrawError,
    VideoDrawOutcome, VideoDrawRequest, VideoLayerError, VideoLayerOptions, VideoLayerStats,
    VideoUploadError,
};
use crate::planes::{
    ChromaSubsampling, FrameLayoutError, PlanarFrameUpload, PlaneRole, PlaneSource,
    validate_host_plane,
};
use crate::program::{FULLSCREEN_TRIANGLE, ShaderPrecision, VideoProgram, VideoPrograms};
use crate::ring::{SlotForWrite, UploadRing, WritableSlot};
use crate::textures::PlaneTextureSet;
use crate::upload_plan::{PlaneUploadPlan, RowLengthSupport, plan_plane_upload};

/// Видеослой GLES2.
#[derive(Debug)]
pub struct VideoLayerRenderer {
    owner: ContextId,
    programs: VideoPrograms,
    vertex_buffer: glow::NativeBuffer,
    ring: UploadRing<PlaneTextureSet, GpuFence>,
    plane_format: PlaneTextureFormat,
    row_length: RowLengthSupport,
    max_texture_size: u32,
    stats: VideoLayerStats,
    destroyed: bool,
}

impl VideoLayerRenderer {
    /// Собрать программу и буфер вершин. Текстуры кольца создаются при первой загрузке.
    ///
    /// # Errors
    /// [`VideoLayerError`]: контекст недоступен, шейдер не собрался, нет буфера.
    pub fn new(
        context: &Gles2Context,
        options: VideoLayerOptions,
    ) -> Result<Self, VideoLayerError> {
        let capabilities = context.capabilities();
        let plane_format = match options.plane_format {
            PlaneFormatPreference::BestAvailable
                if capabilities.gl_extensions().contains("GL_EXT_texture_rg") =>
            {
                PlaneTextureFormat::RedExt
            }
            PlaneFormatPreference::BestAvailable | PlaneFormatPreference::LuminanceOnly => {
                PlaneTextureFormat::Luminance
            }
        };
        let row_length = match options.row_length {
            RowLengthPreference::BestAvailable
                if capabilities.tiers().conveniences.unpack_subimage =>
            {
                RowLengthSupport::UnpackRowLength
            }
            RowLengthPreference::BestAvailable | RowLengthPreference::StrideWideTextures => {
                RowLengthSupport::StrideWideTexture
            }
        };
        let max_texture_size = capabilities.max_texture_size();
        let programs = VideoPrograms::build(context)?;
        let gl = VideoGl::for_context(context).map_err(VideoLayerError::GlAccess)?;
        let vertex_buffer = match gl.create_buffer() {
            Ok(buffer) => buffer,
            Err(error) => {
                programs.delete(context);
                return Err(VideoLayerError::Gl(error));
            }
        };
        gl.upload_static_vertices(vertex_buffer, &FULLSCREEN_TRIANGLE);
        Ok(Self {
            owner: context.id(),
            programs,
            vertex_buffer,
            ring: UploadRing::new(options.upload_slots),
            plane_format,
            row_length,
            max_texture_size,
            stats: VideoLayerStats::default(),
            destroyed: false,
        })
    }

    /// Точность фрагментного шейдера (без `highp` номер текселя точен до ширины 2048).
    #[must_use]
    pub const fn shader_precision(&self) -> ShaderPrecision {
        self.programs.precision
    }

    /// Формат текстур плоскостей, который выбран для этого драйвера.
    #[must_use]
    pub const fn plane_texture_format(&self) -> PlaneTextureFormat {
        self.plane_format
    }

    /// Накопленные счётчики.
    #[must_use]
    pub const fn stats(&self) -> VideoLayerStats {
        self.stats
    }

    /// Сколько слотов кольца уже держат текстуры (не больше числа слотов).
    #[must_use]
    pub fn allocated_slots(&self) -> usize {
        self.ring.allocated_slots()
    }

    /// Загрузить кадр (до `begin_frame`).
    ///
    /// Тот же ключ, что у текущего кадра, — повтор без загрузки. Иначе кадр копируется в
    /// свободный слот кольца; свободного нет — `Busy`, байты не читаются, текущий кадр прежний.
    ///
    /// # Errors
    /// [`VideoUploadError`]: чужой или недоступный контекст, кадр не подходит к раскладке,
    /// источник DMA-BUF (ещё не реализован), текстура больше лимита драйвера, отказ fence или GL.
    /// После ошибки посреди записи текущим остаётся прежний кадр.
    pub fn upload(
        &mut self,
        context: &Gles2Context,
        frame: &PlanarFrameUpload<'_>,
    ) -> Result<UploadOutcome, VideoUploadError> {
        if context.id() != self.owner {
            return Err(VideoUploadError::ForeignContext);
        }
        if self.ring.current_key() == Some(frame.key) {
            self.stats.repeated_frames += 1;
            return Ok(UploadOutcome::AlreadyCurrent);
        }
        // Проверки кадра — до выбора слота: отказ не трогает кольцо.
        let prepared = self.prepare_planes(frame)?;
        let slot = match self
            .ring
            .slot_for_write()
            .map_err(VideoUploadError::Fence)?
        {
            SlotForWrite::Free(slot) => slot,
            SlotForWrite::Busy(reason) => {
                self.stats.busy_rejections += 1;
                return Ok(UploadOutcome::Busy(reason));
            }
        };
        let gl = VideoGl::for_context(context).map_err(VideoUploadError::GlAccess)?;
        match self.write_slot(gl, slot, &prepared) {
            Ok(report) => {
                self.ring.commit_upload(slot, frame.key);
                self.record_upload(report);
                Ok(UploadOutcome::Uploaded(report))
            }
            Err(error) => {
                self.ring.abandon_upload(slot);
                Err(error)
            }
        }
    }

    /// Проверить плоскости кадра и построить планы загрузки.
    fn prepare_planes<'frame>(
        &self,
        frame: &PlanarFrameUpload<'frame>,
    ) -> Result<PreparedFrame<'frame>, VideoUploadError> {
        if frame.luma_extent.is_empty() {
            return Err(VideoUploadError::Layout(FrameLayoutError::EmptyFrame));
        }
        let [(luma_role, luma), (u_role, chroma_u), (v_role, chroma_v)] = frame.planes();
        Ok(PreparedFrame {
            chroma: frame.layout.chroma,
            planes: [
                self.prepare_plane(frame, luma_role, luma)?,
                self.prepare_plane(frame, u_role, chroma_u)?,
                self.prepare_plane(frame, v_role, chroma_v)?,
            ],
        })
    }

    /// Проверить одну плоскость и построить её план загрузки.
    fn prepare_plane<'frame>(
        &self,
        frame: &PlanarFrameUpload<'frame>,
        role: PlaneRole,
        source: PlaneSource<'frame>,
    ) -> Result<PreparedPlane<'frame>, VideoUploadError> {
        let rows = match source {
            PlaneSource::HostRows(rows) => rows,
            PlaneSource::DmaBuf(_) => {
                return Err(VideoUploadError::PlaneSourceUnsupported {
                    role,
                    kind: source.kind(),
                });
            }
        };
        validate_host_plane(frame, role, &rows).map_err(VideoUploadError::Layout)?;
        let bytes_per_sample = frame.layout.depth.bytes_per_sample();
        let visible = frame.expected_extent(role);
        let plan =
            plan_plane_upload(&rows, visible, bytes_per_sample, self.row_length).map_err(|_| {
                VideoUploadError::Layout(FrameLayoutError::StrideNotSampleAligned {
                    role,
                    stride: rows.stride(),
                    bytes_per_sample,
                })
            })?;
        if plan.texture.width > self.max_texture_size || plan.texture.height > self.max_texture_size
        {
            return Err(VideoUploadError::PlaneTooLarge {
                role,
                width: plan.texture.width,
                height: plan.texture.height,
                max: self.max_texture_size,
            });
        }
        Ok(PreparedPlane {
            plan,
            bytes: rows.bytes(),
        })
    }

    /// Выделить (если нужно) и заполнить текстуры слота.
    fn write_slot(
        &mut self,
        gl: VideoGl<'_>,
        slot: WritableSlot,
        prepared: &PreparedFrame<'_>,
    ) -> Result<UploadReport, VideoUploadError> {
        let plane_format = self.plane_format;
        let ring_slot = self
            .ring
            .slot_mut(slot)
            .ok_or(VideoUploadError::RingSlotUnavailable)?;
        let textures = match &mut ring_slot.textures {
            Some(textures) => textures,
            empty @ None => empty
                .insert(PlaneTextureSet::create(gl, plane_format).map_err(VideoUploadError::Gl)?),
        };
        let allocation = textures
            .ensure_allocated(gl, plane_format, &prepared.plans())
            .map_err(VideoUploadError::Gl)?;
        let mut report = UploadReport {
            upload_calls: 0,
            copied_bytes: 0,
            allocated_textures: allocation.allocated_textures,
        };
        for (plane_texture, plane) in textures.planes.iter_mut().zip(&prepared.planes) {
            for call in plane.plan.calls() {
                gl.upload_plane_rows(plane_texture.texture, plane_format, call, plane.bytes)
                    .map_err(VideoUploadError::Gl)?;
                report.upload_calls += 1;
            }
            report.copied_bytes += plane.plan.copied_bytes();
            plane_texture.visible = plane.plan.visible;
        }
        textures.chroma = Some(prepared.chroma);
        Ok(report)
    }

    fn record_upload(&mut self, report: UploadReport) {
        let as_u64 = |value: usize| u64::try_from(value).unwrap_or(u64::MAX);
        self.stats.frames_uploaded += 1;
        self.stats.planes_uploaded += 3;
        self.stats.upload_calls += u64::from(report.upload_calls);
        self.stats.copied_bytes += as_u64(report.copied_bytes);
        self.stats.texture_allocations += u64::from(report.allocated_textures);
    }

    /// Нарисовать текущий кадр внутри кадра показа.
    ///
    /// Итог — как у видеопрохода wgpu: цель залита непрозрачным чёрным, кадр нарисован в каждом
    /// видимом прямоугольнике. Заливается только то, куда кадр не рисует (дешевле на слабом GPU). Видео пишет альфу 1. Состояние GL после вызова:
    /// VAO не тронут (контракт слоя кадра), scissor выключен, активен текстурный блок 0, viewport
    /// снова на всю цель.
    ///
    /// # Errors
    /// [`VideoDrawError`]: чужой контекст, прямоугольник вне координат GL.
    pub fn draw(
        &mut self,
        frame: &ActiveFrame<'_>,
        request: &VideoDrawRequest<'_>,
    ) -> Result<VideoDrawOutcome, VideoDrawError> {
        if frame.context_id() != self.owner {
            return Err(VideoDrawError::ForeignContext);
        }
        let target = frame.size();
        let target_height = target.height();
        let target_rect = PixelRect {
            x: 0,
            y: 0,
            width: target.width(),
            height: target_height,
        };
        let full_target = to_gl_window_rect(target_rect, target_height)?;
        let viewport = to_gl_window_rect(request.viewport, target_height)?;
        let current = self
            .ring
            .current_textures()
            .and_then(|textures| textures.chroma.map(|chroma| (textures, chroma)));
        // Видео рисует каждый пиксель своих прямоугольников (полосы letterbox — тоже, чёрным),
        // поэтому чёрная заливка нужна только там, куда видео не рисует: под исключениями и
        // вне области. Пиксели те же, что у полной заливки wgpu, а GPU не заливает цель
        // дважды (на T410 полная заливка 1440×900 — ~2 мс).
        let drawn_rects: Vec<PixelRect> = match current {
            Some(_) => request
                .visible_rects
                .iter()
                .filter_map(|rect| rect.intersection(request.viewport))
                .collect(),
            None => Vec::new(),
        };
        let uncovered = uncovered_parts(target_rect, &drawn_rects)
            .into_iter()
            .map(|rect| to_gl_window_rect(rect, target_height))
            .collect::<Result<Vec<_>, _>>()?;
        let scissors = drawn_rects
            .iter()
            .map(|rect| to_gl_window_rect(*rect, target_height))
            .collect::<Result<Vec<_>, _>>()?;

        let gl = VideoGl::for_active_frame(frame);
        gl.set_opaque_video_state();
        for clear_rect in uncovered {
            gl.enable_scissor();
            gl.scissor(clear_rect);
            gl.clear_opaque_black();
        }
        gl.disable_scissor();
        let Some((textures, chroma)) = current else {
            return Ok(VideoDrawOutcome::ClearedWithoutFrame);
        };
        if scissors.is_empty() {
            return Ok(VideoDrawOutcome::ClearedNothingVisible);
        }

        let program = self.programs.for_layout(chroma);
        gl.use_program(Some(program.program));
        set_frame_uniforms(gl, program, textures, request);
        gl.bind_plane_textures(textures.textures());
        gl.bind_vertex_positions(self.vertex_buffer, program.position_attribute);
        gl.viewport(viewport);
        gl.enable_scissor();
        for scissor in &scissors {
            gl.scissor(*scissor);
            gl.draw_fullscreen_triangle();
        }
        gl.disable_scissor();
        gl.unbind_vertex_positions(program.position_attribute);
        gl.use_program(None);
        gl.viewport(full_target);

        self.ring.mark_current_drawn();
        let draw_calls = u32::try_from(scissors.len()).unwrap_or(u32::MAX);
        self.stats.draw_calls += u64::from(draw_calls);
        Ok(VideoDrawOutcome::Drawn { draw_calls })
    }

    /// Работа загрузок и рисований с прошлого вызова отдана драйверу; `fence` поставлен после неё
    /// (`Gles2Context::insert_fence` после `FrameCompositor::render`). Слоты, которые эта работа
    /// трогала, станут доступны для записи, когда fence сработает.
    ///
    /// Звать после каждого кадра показа (и после загрузок без показа): без fence слоты остаются
    /// занятыми и следующая загрузка получит `Busy(AwaitingSubmittedWorkFence)`.
    ///
    /// Fence в `Arc`: один fence кадра показа делят видеослой и учёт кадров декодера у
    /// вызывающего (кадр декодера отпускается по тому же fence).
    pub fn retire_submitted_work(&mut self, fence: Arc<GpuFence>) {
        self.ring.retire(&fence);
    }

    /// Забыть ключи загруженных кадров (ключи вызывающего начались заново: новый декодер). Следующий
    /// `upload` загрузит кадр, даже если ключ совпал; `draw` до него рисует только чёрный.
    pub fn forget_uploaded_frames(&mut self) {
        self.ring.forget_keys();
    }

    /// Удалить GL-объекты видеослоя.
    ///
    /// # Errors
    /// [`VideoLayerError::ForeignContext`] — контекст чужой (объекты не тронуты).
    /// Недоступный контекст не ошибка: объекты умрут вместе с ним, это пишется в лог.
    pub fn destroy(mut self, context: &Gles2Context) -> Result<(), VideoLayerError> {
        if context.id() != self.owner {
            return Err(VideoLayerError::ForeignContext);
        }
        self.destroyed = true;
        let textures = self.ring.take_all_textures();
        match VideoGl::for_context(context) {
            Ok(gl) => {
                for set in textures {
                    set.delete(gl);
                }
                gl.delete_buffer(self.vertex_buffer);
                self.programs.delete(context);
            }
            Err(error) => {
                log::warn!(
                    "gles2-video: context unavailable on destroy ({error}); GL objects die with it"
                );
            }
        }
        Ok(())
    }
}

impl Drop for VideoLayerRenderer {
    fn drop(&mut self) {
        if !self.destroyed {
            log::warn!(
                "video layer dropped without destroy(): {} slot texture set(s), its program and \
                 buffer live until the GLES2 context is destroyed",
                self.ring.allocated_slots()
            );
        }
    }
}

/// Uniform-ы кадра: CPU считает их один раз ([`DrawUniformValues`]), шейдер только умножает.
fn set_frame_uniforms(
    gl: VideoGl<'_>,
    program: &VideoProgram,
    textures: &PlaneTextureSet,
    request: &VideoDrawRequest<'_>,
) {
    let sampling = |plane: &crate::textures::PlaneTexture| {
        let allocated = plane
            .allocated
            .unwrap_or(crate::upload_plan::TextureExtent {
                width: plane.visible.width,
                height: plane.visible.height,
            });
        PlaneSampling::new(plane.visible, allocated)
    };
    let [luma, chroma_u, _] = &textures.planes;
    let values = DrawUniformValues::new(
        sampling(luma),
        sampling(chroma_u),
        request.picture,
        request.orientation,
        request.color,
        request.adjustment,
    );
    let uniforms = &program.uniforms;
    gl.set_uniform_vec4(&uniforms.display_mapping, values.display_mapping);
    for (location, row) in uniforms
        .orientation_rows
        .iter()
        .zip(values.orientation_rows)
    {
        gl.set_uniform_vec3(location, row);
    }
    gl.set_uniform_vec4(&uniforms.luma_coord_scale_min, values.luma_coord_scale_min);
    gl.set_uniform_vec4(&uniforms.luma_coord_max_size, values.luma_coord_max_size);
    if let Some(location) = &uniforms.chroma_coord_scale_min {
        gl.set_uniform_vec4(location, values.chroma_coord_scale_min);
    }
    gl.set_uniform_vec4(
        &uniforms.chroma_coord_max_last,
        values.chroma_coord_max_last,
    );
    if let Some(location) = &uniforms.chroma_texel_size {
        gl.set_uniform_vec2(location, values.chroma_texel_size);
    }
    for (location, row) in uniforms.rgb_rows.iter().zip(values.color.rows) {
        gl.set_uniform_vec4(location, row);
    }
}

/// Плоскость, готовая к загрузке.
struct PreparedPlane<'frame> {
    plan: PlaneUploadPlan,
    bytes: &'frame [u8],
}

/// Три плоскости кадра в порядке Y, U, V.
struct PreparedFrame<'frame> {
    chroma: ChromaSubsampling,
    planes: [PreparedPlane<'frame>; 3],
}

impl PreparedFrame<'_> {
    fn plans(&self) -> [PlaneUploadPlan; 3] {
        self.planes.each_ref().map(|plane| plane.plan.clone())
    }
}
