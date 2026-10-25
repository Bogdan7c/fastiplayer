//! Текстура таблицы HDR → SDR на GPU: одна на видеослой, перестраивается только при смене
//! передаточной функции или опорных уровней (ключ [`HdrLutKey`]), а не на кадр.
//!
//! Перезапись текстуры, которую ещё читает отданная драйверу работа прошлого кадра, корректна по
//! спецификации (GLES 2.0 §3.7.1: новое содержимое видят только команды после вызова); на старом
//! железе это может стоить остановки конвейера, но случается только при смене настроек.

use crate::frame_color::RequiredHdrTable;
use crate::gl_calls::VideoGl;
use crate::hdr::lut::{self, HdrLutKey, HdrLutTable};
use crate::hdr_table_builder::{BuildPoll, BuildRequestOutcome, HdrTableBuilder};
use crate::layer_api::VideoDrawError;
use crate::upload_plan::TextureExtent;

/// Текстура таблицы и ключ того, что в ней лежит.
#[derive(Debug)]
pub(crate) struct HdrTableTexture {
    texture: glow::NativeTexture,
    /// `None` — текстура создана, но заполнить её не удалось.
    loaded: Option<HdrLutKey>,
}

/// Итог `ensure`: какая таблица лежит на GPU и откуда она взялась.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HdrTableReadiness {
    /// Нужная таблица уже на GPU.
    AlreadyLoaded,
    /// Таблицы не было: посчитана и загружена этим вызовом, в потоке рисования (первая таблица
    /// слоя — кадр HDR без неё рисовать нельзя).
    BuiltSynchronously,
    /// Нужная таблица досчитана фоновым потоком и загружена этим вызовом.
    InstalledFromBackground,
    /// На GPU таблица прежних параметров, новая считается в фоне: кадр рисуется со старой.
    /// `ensure` вернул её текстуру; перерисовать кадр нужно, когда сборка закончится.
    OldWhileBuilding,
}

/// Результат `ensure`: текстура, которой рисовать, и откуда она.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EnsuredHdrTable {
    pub(crate) texture: glow::NativeTexture,
    pub(crate) readiness: HdrTableReadiness,
}

impl HdrTableTexture {
    /// Текстура с таблицей `required`.
    ///
    /// - ключ совпал — ничего не делаем (и отзываем лишние запросы фона);
    /// - таблицы на GPU ещё нет — строим синхронно (решение владельца 38: самая первая таблица);
    /// - таблица другого ключа есть — просим фон; готовое забираем без ожидания, иначе рисуем со
    ///   старой ([`HdrTableReadiness::OldWhileBuilding`]).
    ///
    /// # Errors
    /// [`VideoDrawError::HdrTableTooLarge`] — атлас больше лимита драйвера;
    /// [`VideoDrawError::HdrTableTexture`] — драйвер не создал или не выделил текстуру.
    pub(crate) fn ensure(
        slot: &mut Option<Self>,
        builder: &mut HdrTableBuilder,
        gl: VideoGl<'_>,
        required: RequiredHdrTable,
        max_texture_size: u32,
    ) -> Result<EnsuredHdrTable, VideoDrawError> {
        if let Some(current) = slot.as_ref()
            && current.loaded == Some(required.key)
        {
            builder.withdraw();
            return Ok(EnsuredHdrTable {
                texture: current.texture,
                readiness: HdrTableReadiness::AlreadyLoaded,
            });
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
        let has_loaded_table = slot.as_ref().is_some_and(|table| table.loaded.is_some());
        if has_loaded_table
            && let Some(ensured) = Self::try_background(slot, builder, gl, required)?
        {
            return Ok(ensured);
        }
        // Первой таблицы нет (или фон недоступен): строим здесь. Фоновый запрос другого ключа
        // больше не нужен.
        builder.withdraw();
        let contents = HdrLutTable::build(required.transfer, required.levels);
        let texture = Self::upload(slot, gl, extent, required.key, &contents)?;
        Ok(EnsuredHdrTable {
            texture,
            readiness: HdrTableReadiness::BuiltSynchronously,
        })
    }

    /// Фоновый путь для случая «старая таблица на GPU есть». `Ok(None)` — фон не смог (поток не
    /// запустился или умер): вызывающий построит таблицу синхронно, как до решения 38.
    fn try_background(
        slot: &mut Option<Self>,
        builder: &mut HdrTableBuilder,
        gl: VideoGl<'_>,
        required: RequiredHdrTable,
    ) -> Result<Option<EnsuredHdrTable>, VideoDrawError> {
        let Some(old_texture) = slot.as_ref().map(|table| table.texture) else {
            return Ok(None);
        };
        if let BuildRequestOutcome::SpawnFailed(error) = builder.request(required) {
            log::warn!("gles2-video: HDR table worker did not start ({error}); building inline");
            return Ok(None);
        }
        match builder.poll() {
            BuildPoll::Ready(built) => {
                let extent = TextureExtent {
                    width: lut::atlas_width(),
                    height: lut::atlas_height(),
                };
                let texture = Self::upload(slot, gl, extent, built.key, &built.table)?;
                Ok(Some(EnsuredHdrTable {
                    texture,
                    readiness: HdrTableReadiness::InstalledFromBackground,
                }))
            }
            BuildPoll::WorkerLost => {
                log::warn!("gles2-video: HDR table worker died; building inline");
                Ok(None)
            }
            BuildPoll::NotReady | BuildPoll::Superseded => Ok(Some(EnsuredHdrTable {
                texture: old_texture,
                readiness: HdrTableReadiness::OldWhileBuilding,
            })),
        }
    }

    /// Загрузить `contents` в текстуру слоя (создать её при первой загрузке) и записать ключ.
    fn upload(
        slot: &mut Option<Self>,
        gl: VideoGl<'_>,
        extent: TextureExtent,
        key: HdrLutKey,
        contents: &HdrLutTable,
    ) -> Result<glow::NativeTexture, VideoDrawError> {
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
        gl.allocate_hdr_lut_texture(table.texture, extent, &contents.rgba)
            .map_err(VideoDrawError::HdrTableTexture)?;
        table.loaded = Some(key);
        Ok(table.texture)
    }

    /// Удалить текстуру.
    pub(crate) fn delete(self, gl: VideoGl<'_>) {
        gl.delete_texture(self.texture);
    }
}
