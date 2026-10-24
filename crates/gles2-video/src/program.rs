//! Программы видеослоя: планарный YUV → RGB, letterbox, ориентация, цвет.
//!
//! # 8 бит
//! Формулы — строгий паритет с `render-wgpu-video/shaders/host_yuv420_8bit_to_rgba.wgsl`:
//! - треугольник на весь viewport, `viewport_uv` с началом слева сверху (`uv.y = 0` — верхняя
//!   строка, как у wgpu: у GL clip `y = +1` — тоже верх области);
//! - `display_uv = viewport_uv × uv_scale + uv_offset`; вне `[0, 1]` — чёрный (полосы
//!   letterbox), проверка до поворота;
//! - поворот: `source_uv` = аффинное преобразование `display_uv`;
//! - яркость — билинейная выборка в `source_uv`;
//! - цветность по прореженной оси — центр текселя `floor(тексель_яркости / 2)`, то есть
//!   фактически nearest co-sited (как wgpu-хост); по непрореженной оси — билинейная, как яркость;
//! - цвет — одна аффинная матрица ([`crate::color`]) и `clamp`.
//!
//! # 10/12 бит (сессия 06)
//! Паритет с `host_yuv420_16bit_to_sdr.wgsl`: wgpu читает `R16Uint` через `textureLoad`, то есть
//! без фильтрации. Здесь пара байтов LE лежит в `GL_LUMINANCE_ALPHA` с `GL_NEAREST`, код
//! собирает шейдер ([`shaders::PLANE_CODE_FUNCTION`]). Выбор текселя `floor(uv × размер)` делает
//! сама выборка nearest, а цветность wgpu `floor(тексель_яркости / делитель)` — масштаб
//! координаты в uniform-е: вариант на раскладку не нужен, адреса выборок не считаются на пиксель.
//! Дальше — нормализация кодов (`clamp`, [`crate::color::high_bit`]) и матрица; у HDR ещё
//! таблица тон-маппинга ([`crate::hdr`]). Нужен `highp`: без него программы 10/12 бит не
//! собираются, и загрузка таких кадров — типизированный отказ.
//!
//! # Как это уложено под Ironlake (замер T410, сессия 05)
//! Пошаговая версия 8 бит стоила ~19 мс GPU на кадр 1440×900 при бюджете 16,6. По ассемблеру
//! crocus основное время — три выборки текстур по очереди (адрес каждой считался в шейдере) и
//! ветвления по uniform-ам. Поэтому:
//! - `display_uv` и `source_uv` — аффинные функции позиции, их считает вершинный шейдер, а
//!   растеризатор интерполирует (совпадает с расчётом на пиксель до округления `f32`);
//! - прореженность цветности 8 бит — свойство раскладки: по варианту программы на раскладку
//!   (`#define`), без ветвлений на пиксель; HDR — тоже `#define`, а не ветвление;
//! - вместо делений на размер текстуры — умножение на готовые величины из uniform-ов.
//!
//! Отличия от WGSL, вызванные GLSL ES 1.00, а не выбором:
//! - нет `textureDimensions`: размеры плоскостей приходят uniform-ами;
//! - текстура может быть шире видимой части (stride без `EXT_unpack_subimage`): координата
//!   выборки упирается в центр последнего видимого текселя. Для текстуры ровно в видимую ширину
//!   это то же самое, что `CLAMP_TO_EDGE` у wgpu, а для широкой — не даёт подмешать мусор хвоста;
//! - целочисленные `u32` заменены на `floor()` над `float` (точно для целых значений).
//!
//! Точность: координаты и цвет считаются в `highp`, где он есть (сессия 04: `mediump` на
//! radeonsi — настоящий fp16). Без `highp` номер текселя точен только до ширины 2048 — это
//! видит [`ShaderPrecision::exact_texel_limit`]. Uniform-ов во фрагментном шейдере: 8 векторов у
//! 8 бит, 14 у HDR (минимум GLES 2.0 — 16); сэмплеров 3 и 4 (минимум 8).

mod shaders;

use gles2_egui::{Es100ProgramSources, Gles2Capabilities, Gles2Context};

use crate::VideoLayerError;
use crate::gl_calls::{HDR_LUT_TEXTURE_UNIT, VideoGl};
use crate::planes::ChromaSubsampling;

/// Точность фрагментного шейдера видеослоя.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderPrecision {
    /// `highp` (fp32): формулы как у wgpu.
    High,
    /// Только `mediump` (fp16): номер текселя точен до ширины 2048.
    Medium,
}

impl ShaderPrecision {
    /// Точность, которую видеослой выберет на этом драйвере: `highp`, если он есть во
    /// фрагментном шейдере.
    #[must_use]
    pub fn for_capabilities(capabilities: &Gles2Capabilities) -> Self {
        if capabilities.fragment_high_float_precision_bits() > 0 {
            Self::High
        } else {
            Self::Medium
        }
    }

    /// Самая широкая плоскость, для которой номер текселя считается точно.
    #[must_use]
    pub const fn exact_texel_limit(self) -> Option<u32> {
        match self {
            Self::High => None,
            // fp16: 11 бит мантиссы, целые точны до 2048.
            Self::Medium => Some(2048),
        }
    }

    /// Можно ли рисовать 10/12 бит (и HDR → SDR): код до 4095 и тон-маппинг требуют fp32.
    #[must_use]
    pub const fn supports_high_bit_depth(self) -> bool {
        matches!(self, Self::High)
    }
}

/// Какая программа нужна кадру.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProgramVariant {
    /// 8 бит: билинейная выборка, одна матрица; по программе на раскладку.
    EightBit(ChromaSubsampling),
    /// 10/12 бит SDR: пары байтов, nearest, нормализация и матрица.
    HighBitSdr,
    /// 10/12 бит HDR → SDR: то же плюс таблица тон-маппинга.
    HighBitHdrTable,
}

/// Uniform-ы нормализации кодов 10/12 бит.
#[derive(Debug)]
pub(crate) struct CodeNormalizationUniforms {
    pub(crate) scale: glow::NativeUniformLocation,
    pub(crate) offset: glow::NativeUniformLocation,
    pub(crate) min: glow::NativeUniformLocation,
    pub(crate) max: glow::NativeUniformLocation,
}

/// Uniform-ы таблицы HDR → SDR.
#[derive(Debug)]
pub(crate) struct HdrTableUniforms {
    pub(crate) layout: glow::NativeUniformLocation,
    pub(crate) texel_size: glow::NativeUniformLocation,
    pub(crate) decode: glow::NativeUniformLocation,
}

/// Uniform-ы программы.
#[derive(Debug)]
pub(crate) struct VideoProgramUniforms {
    pub(crate) display_mapping: glow::NativeUniformLocation,
    pub(crate) orientation_rows: [glow::NativeUniformLocation; 2],
    pub(crate) luma_coord_scale_min: glow::NativeUniformLocation,
    pub(crate) luma_coord_max_size: glow::NativeUniformLocation,
    /// Билинейная цветность 8 бит — только у раскладок с непрореженной осью (4:2:2 по
    /// вертикали, 4:4:4); у 4:2:0 компилятор выкидывает uniform. У 10/12 бит есть всегда.
    pub(crate) chroma_coord_scale_min: Option<glow::NativeUniformLocation>,
    pub(crate) chroma_coord_max_last: glow::NativeUniformLocation,
    /// Центры текселей цветности — только у прореженных раскладок 8 бит (4:2:0, 4:2:2).
    pub(crate) chroma_texel_size: Option<glow::NativeUniformLocation>,
    pub(crate) rgb_rows: [glow::NativeUniformLocation; 3],
    /// Только у программ 10/12 бит.
    pub(crate) code_normalization: Option<CodeNormalizationUniforms>,
    /// Только у программы HDR.
    pub(crate) hdr_table: Option<HdrTableUniforms>,
}

/// Собранная программа одного варианта.
#[derive(Debug)]
pub(crate) struct VideoProgram {
    pub(crate) program: glow::NativeProgram,
    pub(crate) position_attribute: u32,
    pub(crate) uniforms: VideoProgramUniforms,
}

/// Программы 10/12 бит (собираются только при `highp`).
#[derive(Debug)]
struct HighBitPrograms {
    sdr: VideoProgram,
    hdr_table: VideoProgram,
}

/// Все программы одного контекста.
#[derive(Debug)]
pub(crate) struct VideoPrograms {
    yuv420: VideoProgram,
    yuv422: VideoProgram,
    yuv444: VideoProgram,
    high_bit: Option<HighBitPrograms>,
    pub(crate) precision: ShaderPrecision,
}

impl VideoPrograms {
    /// Компилирует программы всех вариантов, которые доступны при точности драйвера; отказ
    /// любой удаляет уже собранные.
    pub(crate) fn build(context: &Gles2Context) -> Result<Self, VideoLayerError> {
        let precision = ShaderPrecision::for_capabilities(context.capabilities());
        let mut built = Vec::new();
        let mut build = |variant| {
            let program = VideoProgram::build(context, precision, variant).inspect_err(|_| {
                for program in &built {
                    delete_program_logged(context, *program);
                }
            })?;
            built.push(program.program);
            Ok::<_, VideoLayerError>(program)
        };
        let yuv420 = build(ProgramVariant::EightBit(ChromaSubsampling::Yuv420))?;
        let yuv422 = build(ProgramVariant::EightBit(ChromaSubsampling::Yuv422))?;
        let yuv444 = build(ProgramVariant::EightBit(ChromaSubsampling::Yuv444))?;
        let high_bit = if precision.supports_high_bit_depth() {
            Some(HighBitPrograms {
                sdr: build(ProgramVariant::HighBitSdr)?,
                hdr_table: build(ProgramVariant::HighBitHdrTable)?,
            })
        } else {
            None
        };
        Ok(Self {
            yuv420,
            yuv422,
            yuv444,
            high_bit,
            precision,
        })
    }

    /// Программа варианта; `None` — вариант 10/12 бит на драйвере без `highp`.
    pub(crate) fn for_variant(&self, variant: ProgramVariant) -> Option<&VideoProgram> {
        match variant {
            ProgramVariant::EightBit(ChromaSubsampling::Yuv420) => Some(&self.yuv420),
            ProgramVariant::EightBit(ChromaSubsampling::Yuv422) => Some(&self.yuv422),
            ProgramVariant::EightBit(ChromaSubsampling::Yuv444) => Some(&self.yuv444),
            ProgramVariant::HighBitSdr => self.high_bit.as_ref().map(|programs| &programs.sdr),
            ProgramVariant::HighBitHdrTable => {
                self.high_bit.as_ref().map(|programs| &programs.hdr_table)
            }
        }
    }

    /// Удалить все программы.
    pub(crate) fn delete(&self, context: &Gles2Context) {
        let high_bit = self
            .high_bit
            .iter()
            .flat_map(|programs| [&programs.sdr, &programs.hdr_table]);
        for program in [&self.yuv420, &self.yuv422, &self.yuv444]
            .into_iter()
            .chain(high_bit)
        {
            delete_program_logged(context, program.program);
        }
    }
}

fn delete_program_logged(context: &Gles2Context, program: glow::NativeProgram) {
    if let Err(error) = context.delete_program(program) {
        log::warn!("gles2-video: failed to delete program: {error}");
    }
}

impl VideoProgram {
    /// Компилирует, линкует, находит входы и привязывает сэмплеры к блокам.
    fn build(
        context: &Gles2Context,
        precision: ShaderPrecision,
        variant: ProgramVariant,
    ) -> Result<Self, VideoLayerError> {
        let fragment_source = fragment_shader_source(precision, variant);
        let program = context
            .compile_es100_program(Es100ProgramSources {
                vertex: shaders::VERTEX_SHADER,
                fragment: &fragment_source,
            })
            .map_err(VideoLayerError::Shader)?;
        // Программа уже создана: отказ поиска входов удаляет её, чтобы не оставить объект.
        Self::locate(context, program, variant)
            .inspect_err(|_| delete_program_logged(context, program))
    }

    fn locate(
        context: &Gles2Context,
        program: glow::NativeProgram,
        variant: ProgramVariant,
    ) -> Result<Self, VideoLayerError> {
        let gl = VideoGl::for_context(context).map_err(VideoLayerError::GlAccess)?;
        let uniform = |name: &'static str| {
            gl.uniform_location(program, name)
                .ok_or(VideoLayerError::MissingShaderInput { name })
        };
        // Вход нужен этому варианту — обязателен; не нужен — компилятор вправе его выкинуть.
        let uniform_if = |name: &'static str, needed: bool| {
            if needed {
                uniform(name).map(Some)
            } else {
                Ok(gl.uniform_location(program, name))
            }
        };
        let needs = UniformNeeds::of(variant);
        let position_attribute = gl
            .attribute_location(program, "a_position")
            .ok_or(VideoLayerError::MissingShaderInput { name: "a_position" })?;
        let mut samplers = vec![
            (uniform("u_luma_plane")?, 0),
            (uniform("u_chroma_u_plane")?, 1),
            (uniform("u_chroma_v_plane")?, 2),
        ];
        let code_normalization = if needs.code_normalization {
            Some(CodeNormalizationUniforms {
                scale: uniform("u_code_scale")?,
                offset: uniform("u_code_offset")?,
                min: uniform("u_normalized_min")?,
                max: uniform("u_normalized_max")?,
            })
        } else {
            None
        };
        let hdr_table = if needs.hdr_table {
            samplers.push((uniform("u_hdr_table")?, HDR_LUT_TEXTURE_UNIT));
            Some(HdrTableUniforms {
                layout: uniform("u_hdr_table_layout")?,
                texel_size: uniform("u_hdr_table_texel_size")?,
                decode: uniform("u_hdr_table_decode")?,
            })
        } else {
            None
        };
        let uniforms = VideoProgramUniforms {
            display_mapping: uniform("u_display_mapping")?,
            orientation_rows: [
                uniform("u_orientation_row0")?,
                uniform("u_orientation_row1")?,
            ],
            luma_coord_scale_min: uniform("u_luma_coord_scale_min")?,
            luma_coord_max_size: uniform("u_luma_coord_max_size")?,
            chroma_coord_scale_min: uniform_if(
                "u_chroma_coord_scale_min",
                needs.chroma_coord_scale_min,
            )?,
            chroma_coord_max_last: uniform("u_chroma_coord_max_last")?,
            chroma_texel_size: uniform_if("u_chroma_texel_size", needs.chroma_texel_size)?,
            rgb_rows: [
                uniform("u_rgb_row0")?,
                uniform("u_rgb_row1")?,
                uniform("u_rgb_row2")?,
            ],
            code_normalization,
            hdr_table,
        };
        // Сэмплеры привязываются к блокам один раз: значение uniform-а живёт в программе.
        gl.use_program(Some(program));
        for (sampler, unit) in &samplers {
            gl.set_uniform_int(sampler, *unit);
        }
        gl.use_program(None);
        Ok(Self {
            program,
            position_attribute,
            uniforms,
        })
    }
}

/// Какие необязательные входы обязан иметь вариант.
struct UniformNeeds {
    chroma_coord_scale_min: bool,
    chroma_texel_size: bool,
    code_normalization: bool,
    hdr_table: bool,
}

impl UniformNeeds {
    const fn of(variant: ProgramVariant) -> Self {
        match variant {
            ProgramVariant::EightBit(chroma) => {
                let (subsampled_x, subsampled_y) = chroma_subsampled_axes(chroma);
                Self {
                    chroma_coord_scale_min: !(subsampled_x && subsampled_y),
                    chroma_texel_size: subsampled_x || subsampled_y,
                    code_normalization: false,
                    hdr_table: false,
                }
            }
            ProgramVariant::HighBitSdr | ProgramVariant::HighBitHdrTable => Self {
                chroma_coord_scale_min: true,
                chroma_texel_size: false,
                code_normalization: true,
                hdr_table: matches!(variant, ProgramVariant::HighBitHdrTable),
            },
        }
    }
}

/// Исходник фрагментного шейдера: `#version 100` обязан быть первой строкой, поэтому точность и
/// `#define` варианта вставляются после неё.
fn fragment_shader_source(precision: ShaderPrecision, variant: ProgramVariant) -> String {
    let precision_line = match precision {
        ShaderPrecision::High => "precision highp float;",
        ShaderPrecision::Medium => "precision mediump float;",
    };
    match variant {
        ProgramVariant::EightBit(chroma) => {
            let (subsampled_x, subsampled_y) = chroma_subsampled_axes(chroma);
            let (subsampled_x, subsampled_y) = (u8::from(subsampled_x), u8::from(subsampled_y));
            format!(
                "#version 100\n{precision_line}\n#define CHROMA_SUBSAMPLED_X {subsampled_x}\n\
                 #define CHROMA_SUBSAMPLED_Y {subsampled_y}\n{}",
                shaders::EIGHT_BIT_FRAGMENT_BODY
            )
        }
        ProgramVariant::HighBitSdr | ProgramVariant::HighBitHdrTable => {
            let hdr_table = u8::from(matches!(variant, ProgramVariant::HighBitHdrTable));
            format!(
                "#version 100\n{precision_line}\n#define HDR_TABLE {hdr_table}\n{}{}",
                shaders::PLANE_CODE_FUNCTION,
                shaders::HIGH_BIT_FRAGMENT_BODY
            )
        }
    }
}

/// Прорежена ли цветность по осям `(x, y)`.
pub(crate) const fn chroma_subsampled_axes(chroma: ChromaSubsampling) -> (bool, bool) {
    match chroma {
        ChromaSubsampling::Yuv420 => (true, true),
        ChromaSubsampling::Yuv422 => (true, false),
        ChromaSubsampling::Yuv444 => (false, false),
    }
}

/// Вершины треугольника, накрывающего весь viewport (как у wgpu): `(-1,-1)`, `(3,-1)`, `(-1,3)`.
pub(crate) const FULLSCREEN_TRIANGLE: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
