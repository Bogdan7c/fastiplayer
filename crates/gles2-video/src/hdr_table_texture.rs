//! Текстура таблицы HDR → SDR на GPU: одна на видеослой, перестраивается только при смене
//! передаточной функции или опорных уровней (ключ [`HdrLutKey`]), а не на кадр.
//!
//! Перезапись текстуры, которую ещё читает отданная драйверу работа прошлого кадра, корректна по
//! спецификации (GLES 2.0 §3.7.1: новое содержимое видят только команды после вызова); на старом
//! железе это может стоить остановки конвейера, но случается только при смене настроек.

use crate::frame_color::RequiredHdrTable;
use crate::gl_calls::VideoGl;
use crate::hdr::lut::{self, HdrLutKey, HdrLutTable};
use crate::layer_api::VideoDrawError;
use crate::upload_plan::TextureExtent;

/// Текстура таблицы и ключ того, что в ней лежит.
#[derive(Debug)]
pub(crate) struct HdrTableTexture {
    texture: glow::NativeTexture,
    /// `None` — текстура создана, но заполнить её не удалось.
    loaded: Option<HdrLutKey>,
}

/// Итог `ensure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HdrTableReadiness {
    /// Нужная таблица уже на GPU.
    AlreadyLoaded,
    /// Таблица посчитана и загружена этим вызовом.
    Built,
}

impl HdrTableTexture {
    /// Текстура с таблицей `required`: создать или перестроить, если ключ другой.
    ///
    /// # Errors
    /// [`VideoDrawError::HdrTableTooLarge`] — атлас больше лимита драйвера;
    /// [`VideoDrawError::HdrTableTexture`] — драйвер не создал или не выделил текстуру.
    pub(crate) fn ensure(
        slot: &mut Option<Self>,
        gl: VideoGl<'_>,
        required: RequiredHdrTable,
        max_texture_size: u32,
    ) -> Result<(glow::NativeTexture, HdrTableReadiness), VideoDrawError> {
        if let Some(current) = slot.as_ref()
            && current.loaded == Some(required.key)
        {
            return Ok((current.texture, HdrTableReadiness::AlreadyLoaded));
        }
        let extent = TextureExtent {
            width: lut::atlas_width(),
            height: lut::atlas_height(),
        };
        if extent.width > max_texture_size || extent.height > max_texture_size {
            return Err(VideoDrawError::HdrTableTooLarge {
                width: extent.width,
                height: extent.height,
                max: max_texture_size,
            });
        }
        let table = match slot {
            Some(table) => table,
            empty @ None => empty.insert(Self {
                texture: gl
                    .create_texture()
                    .map_err(VideoDrawError::HdrTableTexture)?,
                loaded: None,
            }),
        };
        // Пока загрузка не удалась, текстура не считается заполненной.
        table.loaded = None;
        let contents = HdrLutTable::build(required.transfer, required.levels);
        gl.allocate_hdr_lut_texture(table.texture, extent, &contents.rgba)
            .map_err(VideoDrawError::HdrTableTexture)?;
        table.loaded = Some(required.key);
        Ok((table.texture, HdrTableReadiness::Built))
    }

    /// Удалить текстуру.
    pub(crate) fn delete(self, gl: VideoGl<'_>) {
        gl.delete_texture(self.texture);
    }
}
