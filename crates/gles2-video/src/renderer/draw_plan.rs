//! План рисования текущего кадра: какой он (загруженный в кольцо или импортированный DMA-BUF),
//! какой программой и с каким цветом рисовать. Всё решается до первой записи в цель.

use crate::color::ColorTransform;
use crate::dma_buf_import::{DmaBufImports, ImportedNv12};
use crate::dma_buf_planar_import::DmaBufPlanarImports;
use crate::frame_color::{FrameColorPlan, RequiredHdrTable, plan_frame_color, plan_nv12_color};
use crate::gl_calls::VideoGl;
use crate::layer_api::{VideoDrawError, VideoDrawRequest};
use crate::planes::PlanarLayout;
use crate::program::{Nv12Program, VideoProgram, VideoPrograms};
use crate::ring::UploadRing;
use crate::textures::PlaneTextureSet;

use gles2_egui::GpuFence;

/// Текущий кадр, готовый к рисованию.
pub(super) enum PlannedFrame<'a> {
    /// Планарный кадр (три плоскости): из кольца загрузок или импортированный из DMA-BUF.
    /// Рисуется одной программой и одним набором uniform-ов в обоих случаях.
    Planar {
        textures: &'a PlaneTextureSet,
        layout: PlanarLayout,
        color: FrameColorPlan,
        program: &'a VideoProgram,
    },
    /// Импортированный DMA-BUF NV12 (две плоскости).
    Nv12 {
        textures: &'a ImportedNv12,
        color: ColorTransform,
        program: &'a Nv12Program,
    },
}

impl PlannedFrame<'_> {
    /// Таблица HDR → SDR, которую обязан иметь кадр (только планарные 10/12 бит).
    pub(super) fn hdr_table(&self) -> Option<RequiredHdrTable> {
        match self {
            Self::Planar { color, .. } => color.hdr_table,
            Self::Nv12 { .. } => None,
        }
    }
}

/// Выбрать текущий кадр и его программу. Импортированный кадр и кадр кольца не бывают текущими
/// одновременно (`VideoLayerRenderer::current_key`), порядок проверки не влияет на результат.
///
/// # Errors
/// Несовместимый цвет (HDR → SDR для 8 бит, в том числе NV12) или нет программы 10/12 бит.
pub(super) fn plan_current_frame<'a>(
    ring: &'a UploadRing<PlaneTextureSet, GpuFence>,
    imports: &'a DmaBufImports,
    planar_imports: &'a DmaBufPlanarImports,
    programs: &'a VideoPrograms,
    request: &VideoDrawRequest<'_>,
) -> Result<Option<PlannedFrame<'a>>, VideoDrawError> {
    if let Some(textures) = imports.current_textures() {
        return Ok(Some(PlannedFrame::Nv12 {
            textures,
            color: plan_nv12_color(request.color)?,
            program: &programs.nv12,
        }));
    }
    // Планарный кадр — импортированный или из кольца — планируется одним путём.
    let Some((textures, layout)) = planar_imports
        .current_textures()
        .or_else(|| ring.current_textures())
        .and_then(|textures| textures.layout.map(|layout| (textures, layout)))
    else {
        return Ok(None);
    };
    let color = plan_frame_color(layout, request.color)?;
    let program = programs.for_variant(color.variant).ok_or(
        VideoDrawError::HighBitDepthNeedsHighPrecision {
            depth: layout.depth,
        },
    )?;
    Ok(Some(PlannedFrame::Planar {
        textures,
        layout,
        color,
        program,
    }))
}

/// Включить программу кадра, выставить uniform-ы и привязать текстуры. Возвращает атрибут
/// позиции программы для общей части прохода.
pub(super) fn activate(
    gl: VideoGl<'_>,
    planned: &PlannedFrame<'_>,
    request: &VideoDrawRequest<'_>,
    hdr_table: Option<glow::NativeTexture>,
) -> u32 {
    match planned {
        PlannedFrame::Planar {
            textures,
            layout,
            color,
            program,
        } => {
            gl.use_program(Some(program.program));
            super::set_frame_uniforms(gl, program, textures, *layout, request, color.uniforms);
            gl.bind_plane_textures(
                textures.textures(),
                textures.format,
                program.chroma_filter,
                hdr_table,
            );
            program.position_attribute
        }
        PlannedFrame::Nv12 {
            textures,
            color,
            program,
        } => {
            gl.use_program(Some(program.program));
            set_nv12_uniforms(gl, program, request, *color);
            gl.bind_nv12_textures(textures.luma, textures.chroma);
            program.position_attribute
        }
    }
}

/// Uniform-ы NV12: letterbox, поворот и матрица цвета. Адресов плоскостей нет: текстуры ровно
/// размера плоскостей, координата выборки — varying как есть.
fn set_nv12_uniforms(
    gl: VideoGl<'_>,
    program: &Nv12Program,
    request: &VideoDrawRequest<'_>,
    color: ColorTransform,
) {
    let [uv_scale_x, uv_scale_y] = request.picture.uv_scale;
    let [uv_offset_x, uv_offset_y] = request.picture.uv_offset;
    gl.set_uniform_vec4(
        &program.display_mapping,
        [uv_scale_x, uv_scale_y, uv_offset_x, uv_offset_y],
    );
    for (location, row) in program
        .orientation_rows
        .iter()
        .zip(request.orientation.display_to_source_rows())
    {
        gl.set_uniform_vec3(location, row);
    }
    for (location, row) in program.rgb_rows.iter().zip(color.rows) {
        gl.set_uniform_vec4(location, row);
    }
}
