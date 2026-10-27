//! Набор текстур одного слота кольца: Y, U, V.
//!
//! Текстура выделяется (`glTexImage2D` без данных) только когда меняются её размер или формат;
//! кадр того же размера пишется в уже выделенную память (`glTexSubImage2D`). Так поток кадров
//! одного видео не выделяет память на кадр.

use crate::gl_calls::{PlaneTextureFormat, VideoGl, VideoGlError};
use crate::planes::{PlanarLayout, PlaneExtent};
use crate::upload_plan::{PlaneUploadPlan, TextureExtent};

/// Текстура одной плоскости.
#[derive(Debug)]
pub(crate) struct PlaneTexture {
    pub(crate) texture: glow::NativeTexture,
    /// Выделенный размер в текселях (может быть шире видимой части).
    pub(crate) allocated: Option<TextureExtent>,
    /// Видимый размер последнего загруженного кадра.
    pub(crate) visible: PlaneExtent,
}

/// Текстуры Y, U, V слота и формат, в котором они выделены.
#[derive(Debug)]
pub(crate) struct PlaneTextureSet {
    pub(crate) planes: [PlaneTexture; 3],
    pub(crate) format: PlaneTextureFormat,
    /// Раскладка последнего загруженного кадра (выбирает вариант программы); `None` до загрузки.
    pub(crate) layout: Option<PlanarLayout>,
}

/// Что сделал `ensure_allocated`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct AllocationReport {
    /// Сколько текстур выделено заново.
    pub(crate) allocated_textures: u32,
}

impl PlaneTextureSet {
    /// Три пустые текстуры (память выделит `ensure_allocated`).
    pub(crate) fn create(
        gl: VideoGl<'_>,
        format: PlaneTextureFormat,
    ) -> Result<Self, VideoGlError> {
        let luma = gl.create_texture()?;
        // Отказ на второй или третьей текстуре удаляет уже созданные: объекты не остаются в
        // контексте без владельца.
        let chroma_u = gl
            .create_texture()
            .inspect_err(|_| gl.delete_texture(luma))?;
        let chroma_v = gl.create_texture().inspect_err(|_| {
            gl.delete_texture(luma);
            gl.delete_texture(chroma_u);
        })?;
        let unallocated = |texture| PlaneTexture {
            texture,
            allocated: None,
            visible: PlaneExtent {
                width: 0,
                height: 0,
            },
        };
        Ok(Self {
            planes: [
                unallocated(luma),
                unallocated(chroma_u),
                unallocated(chroma_v),
            ],
            format,
            layout: None,
        })
    }

    /// Набор из трёх текстур, чьё хранилище — импортированные плоскости DMA-BUF (`R8` у 8 бит,
    /// `GR88` у 10/12 бит).
    ///
    /// Текстура ровно размера видимой плоскости (`allocated == visible`): шаг строки учёл
    /// `EGLImage` (pitch), поэтому хвоста строки в текстуре нет и шейдеру не нужна обрезка.
    /// `format` номинальный ([`imported_plane_format`]): набор не переразмечается через
    /// `ensure_allocated`, импорт заменяет набор целиком.
    pub(crate) fn from_imported_planes(
        textures: [glow::NativeTexture; 3],
        layout: PlanarLayout,
        extents: [PlaneExtent; 3],
    ) -> Self {
        let plane = |texture, visible: PlaneExtent| PlaneTexture {
            texture,
            allocated: Some(TextureExtent {
                width: visible.width,
                height: visible.height,
            }),
            visible,
        };
        let [luma, chroma_u, chroma_v] = textures;
        let [luma_extent, chroma_u_extent, chroma_v_extent] = extents;
        Self {
            planes: [
                plane(luma, luma_extent),
                plane(chroma_u, chroma_u_extent),
                plane(chroma_v, chroma_v_extent),
            ],
            format: imported_plane_format(layout),
            layout: Some(layout),
        }
    }

    /// Выделить память тем текстурам, чей размер или формат не совпадает с планами кадра.
    pub(crate) fn ensure_allocated(
        &mut self,
        gl: VideoGl<'_>,
        format: PlaneTextureFormat,
        plans: &[PlaneUploadPlan; 3],
    ) -> Result<AllocationReport, VideoGlError> {
        let format_changed = self.format != format;
        self.format = format;
        let mut report = AllocationReport::default();
        for (plane, plan) in self.planes.iter_mut().zip(plans) {
            if format_changed || plane.allocated != Some(plan.texture) {
                // Пока выделение не удалось, текстура считается невыделенной.
                plane.allocated = None;
                gl.allocate_plane_texture(plane.texture, format, plan.texture)?;
                plane.allocated = Some(plan.texture);
                report.allocated_textures += 1;
            }
        }
        Ok(report)
    }

    /// Текстуры в порядке Y, U, V.
    pub(crate) fn textures(&self) -> [glow::NativeTexture; 3] {
        self.planes.each_ref().map(|plane| plane.texture)
    }

    /// Удалить текстуры.
    pub(crate) fn delete(self, gl: VideoGl<'_>) {
        for plane in self.planes {
            gl.delete_texture(plane.texture);
        }
    }
}

/// Формат текстур импортированных плоскостей: один канал `R` у 8 бит, пара `(R, G)` у 10/12 бит.
/// Номинальный: хранилище задаёт `EGLImage`, а формат выбирает фильтр и программу кадра.
const fn imported_plane_format(layout: PlanarLayout) -> PlaneTextureFormat {
    if layout.depth.is_high_bit() {
        PlaneTextureFormat::RgExt
    } else {
        PlaneTextureFormat::RedExt
    }
}
