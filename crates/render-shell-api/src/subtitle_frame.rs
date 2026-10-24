//! Субтитры одного кадра в нейтральной форме.
//!
//! Правила рисования (порядок картинок, смешивание «over» с премультиплицированной альфой,
//! 1 пиксель холста = 1 пиксель surface, обрезка видимой областью видео) описаны у
//! wgpu-реализации в `render-wgpu-shell/src/subtitle_overlay/mod.rs` и обязательны для
//! каждого рендера (строгий паритет, решение 7 плана `user/gles2-renderer/`).

use render_core::PictureRect;
use subtitle_raster::SubtitleOverlay;

/// Сдвиг всего слоя субтитров относительно левого верхнего угла картинки видео.
///
/// Единицы — пиксели surface (не точки egui); `x` вправо, `y` вниз. Слой едет целиком одним
/// сдвигом uniform-а: перерастеризация и повторная загрузка атласа не нужны. Решает сдвиг
/// вызывающий (пользовательская позиция, подъём над панелью управления), рендер лишь
/// применяет его.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SubtitleLayerOffset {
    /// Сдвиг вправо в пикселях surface (отрицательный — влево).
    pub x: f32,
    /// Сдвиг вниз в пикселях surface (отрицательный — вверх).
    pub y: f32,
}

impl SubtitleLayerOffset {
    /// Без сдвига: слой лежит ровно на картинке видео.
    pub const NONE: Self = Self { x: 0.0, y: 0.0 };
}

/// Сдвиги групп размещения слоя: основная и фиксированная двигаются независимо.
///
/// Нужны режиму «поднять основной диалог над панелью, надписи оставить на месте»; для всех
/// остальных случаев обе группы получают один сдвиг ([`Self::uniform`]). Оба сдвига живут в
/// uniform-е: смена любого из них не перезагружает маски и инстансы.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SubtitleLayerOffsets {
    /// Сдвиг основной группы (SRT/WebVTT, ASS-диалог без `\pos`, снизу).
    pub main: SubtitleLayerOffset,
    /// Сдвиг фиксированной группы (ASS с `\pos`/`\move`, верх и середина).
    pub fixed: SubtitleLayerOffset,
}

impl SubtitleLayerOffsets {
    /// Обе группы на месте.
    pub const NONE: Self = Self::uniform(SubtitleLayerOffset::NONE);

    /// Один сдвиг для обеих групп: слой едет целиком.
    #[must_use]
    pub const fn uniform(offset: SubtitleLayerOffset) -> Self {
        Self {
            main: offset,
            fixed: offset,
        }
    }
}

/// Субтитры одного кадра: растеризованный overlay и где на экране лежит картинка видео.
#[derive(Debug, Clone, Copy)]
pub struct SubtitleOverlayFrame<'frame> {
    /// Результат растеризатора; холст = округлённый размер `picture_rect`.
    pub overlay: &'frame SubtitleOverlay,
    /// Картинка видео без чёрных полос в пикселях surface; её округлённый левый верхний
    /// угол — точка, куда ложится (0, 0) холста.
    pub picture_rect: PictureRect,
    /// Сдвиги групп слоя относительно `picture_rect` в пикселях surface (позиция по выбору
    /// пользователя, подъём над панелью управления). Меняется только uniform.
    /// `SubtitleLayerOffsets::uniform` двигает слой целиком.
    pub layer_offsets: SubtitleLayerOffsets,
}
