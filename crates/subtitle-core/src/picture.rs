//! Нейтральная модель растровой (картиночной) реплики: PGS сегодня, VobSub/DVB — позже.
//!
//! Реплика хранит картинку в СЖАТОМ виде (палитровый индексный поток + палитра): так память
//! под длинный фильм остаётся малой, а распаковка происходит только для показываемых реплик.
//! Сам crate не умеет распаковывать — это делает crate формата (`subtitle-pgs`); здесь только
//! данные и их инварианты.

use std::num::NonZeroU32;
use std::sync::Arc;

/// Размер системы координат, в которой автор рисовал субтитры (для PGS — размер видео из PCS).
///
/// Позиции и размеры реплики заданы именно в этих координатах; при показе их масштабируют
/// к фактическому кадру.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PictureReferenceSize {
    width: NonZeroU32,
    height: NonZeroU32,
}

impl PictureReferenceSize {
    pub const fn new(width: NonZeroU32, height: NonZeroU32) -> Self {
        Self { width, height }
    }

    pub const fn width(self) -> NonZeroU32 {
        self.width
    }

    pub const fn height(self) -> NonZeroU32 {
        self.height
    }
}

/// Позиция левого верхнего угла в пикселях (в системе координат, указанной рядом).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PicturePosition {
    pub x: u32,
    pub y: u32,
}

/// Непустой прямоугольник в пикселях. Инвариант `width, height > 0` задан типами.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PictureRect {
    x: u32,
    y: u32,
    width: NonZeroU32,
    height: NonZeroU32,
}

impl PictureRect {
    pub const fn new(x: u32, y: u32, width: NonZeroU32, height: NonZeroU32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn x(self) -> u32 {
        self.x
    }

    pub const fn y(self) -> u32 {
        self.y
    }

    pub const fn width(self) -> NonZeroU32 {
        self.width
    }

    pub const fn height(self) -> NonZeroU32 {
        self.height
    }

    /// Пересечение двух прямоугольников; `None`, если оно пусто. Вычисления в `u64`,
    /// поэтому переполнения на краю диапазона `u32` нет.
    pub fn intersection(self, other: Self) -> Option<Self> {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = (u64::from(self.x) + u64::from(self.width.get()))
            .min(u64::from(other.x) + u64::from(other.width.get()));
        let bottom = (u64::from(self.y) + u64::from(self.height.get()))
            .min(u64::from(other.y) + u64::from(other.height.get()));
        let width = u32::try_from(right.checked_sub(u64::from(left))?).ok()?;
        let height = u32::try_from(bottom.checked_sub(u64::from(top))?).ok()?;
        Some(Self::new(
            left,
            top,
            NonZeroU32::new(width)?,
            NonZeroU32::new(height)?,
        ))
    }

    /// `true`, если `inner` целиком лежит внутри `self`.
    pub fn contains(self, inner: Self) -> bool {
        self.intersection(inner) == Some(inner)
    }
}

/// Способ кодирования индексной картинки. Формат-нейтральный контейнер: новые форматы
/// (например VobSub) добавляются вариантами, не ломая потребителей.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PictureEncoding {
    /// Построчный RLE Blu-ray (HDMV) в том виде, как он лежит в сегменте ODS.
    HdmvRle(Arc<[u8]>),
}

/// Палитровая картинка в сжатом виде.
///
/// Картинка общая для нескольких реплик (обновление только палитры переиспользует её),
/// поэтому реплики держат её через [`Arc`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedPicture {
    width: NonZeroU32,
    height: NonZeroU32,
    encoding: PictureEncoding,
}

impl IndexedPicture {
    pub const fn new(width: NonZeroU32, height: NonZeroU32, encoding: PictureEncoding) -> Self {
        Self {
            width,
            height,
            encoding,
        }
    }

    pub const fn width(&self) -> NonZeroU32 {
        self.width
    }

    pub const fn height(&self) -> NonZeroU32 {
        self.height
    }

    pub const fn encoding(&self) -> &PictureEncoding {
        &self.encoding
    }

    /// Размер сжатых данных в байтах (для учёта памяти).
    pub fn encoded_len(&self) -> usize {
        match &self.encoding {
            PictureEncoding::HdmvRle(bytes) => bytes.len(),
        }
    }

    /// Прямоугольник всей картинки в её собственных координатах.
    pub const fn full_rect(&self) -> PictureRect {
        PictureRect::new(0, 0, self.width, self.height)
    }
}

/// Цвет палитры: RGB без премультипликации (straight alpha), 8 бит на канал.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaletteColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl PaletteColor {
    /// Полностью прозрачный цвет: им заполняются не заданные автором записи палитры.
    pub const TRANSPARENT: Self = Self {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 0,
    };
}

/// Палитра из 256 записей (индекс — байт из индексной картинки).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PicturePalette {
    entries: [PaletteColor; Self::LEN],
}

impl PicturePalette {
    /// Число записей палитры.
    pub const LEN: usize = 256;

    /// Размер палитры в байтах (для учёта памяти).
    pub const BYTE_LEN: usize = Self::LEN * std::mem::size_of::<PaletteColor>();

    pub const fn new(entries: [PaletteColor; Self::LEN]) -> Self {
        Self { entries }
    }

    /// Палитра, где все записи прозрачны.
    pub const fn transparent() -> Self {
        Self::new([PaletteColor::TRANSPARENT; Self::LEN])
    }

    /// Цвет по индексу (индекс `u8` всегда в пределах 256 записей).
    pub fn color(&self, index: u8) -> PaletteColor {
        self.entries[usize::from(index)]
    }

    pub const fn entries(&self) -> &[PaletteColor; Self::LEN] {
        &self.entries
    }
}

/// Пометка «принудительная» (forced) у объекта: показывается даже при выключенных субтитрах.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PictureForcedFlag {
    Forced,
    Regular,
}

/// Нарушение инварианта растровой реплики.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidPictureCue {
    /// Видимая область выходит за границы картинки.
    #[error("visible region {visible_region:?} is outside the {width}x{height} picture")]
    VisibleRegionOutsidePicture {
        visible_region: PictureRect,
        width: u32,
        height: u32,
    },
}

/// Растровая реплика: сжатая картинка, палитра и то, какая её часть и где видна.
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitlePictureCue {
    reference_size: PictureReferenceSize,
    origin: PicturePosition,
    visible_region: PictureRect,
    picture: Arc<IndexedPicture>,
    palette: Arc<PicturePalette>,
    forced: PictureForcedFlag,
}

impl SubtitlePictureCue {
    /// Создаёт реплику.
    ///
    /// - `origin` — позиция левого верхнего угла ВИДИМОЙ области в координатах
    ///   `reference_size`;
    /// - `visible_region` — прямоугольник в координатах самой картинки (после обрезки и
    ///   отсечения окном); обязан лежать внутри картинки.
    pub fn new(
        reference_size: PictureReferenceSize,
        origin: PicturePosition,
        visible_region: PictureRect,
        picture: Arc<IndexedPicture>,
        palette: Arc<PicturePalette>,
        forced: PictureForcedFlag,
    ) -> Result<Self, InvalidPictureCue> {
        if !picture.full_rect().contains(visible_region) {
            return Err(InvalidPictureCue::VisibleRegionOutsidePicture {
                visible_region,
                width: picture.width().get(),
                height: picture.height().get(),
            });
        }
        Ok(Self {
            reference_size,
            origin,
            visible_region,
            picture,
            palette,
            forced,
        })
    }

    pub const fn reference_size(&self) -> PictureReferenceSize {
        self.reference_size
    }

    pub const fn origin(&self) -> PicturePosition {
        self.origin
    }

    pub const fn visible_region(&self) -> PictureRect {
        self.visible_region
    }

    pub fn picture(&self) -> &Arc<IndexedPicture> {
        &self.picture
    }

    pub fn palette(&self) -> &Arc<PicturePalette> {
        &self.palette
    }

    pub const fn forced(&self) -> PictureForcedFlag {
        self.forced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("test value is non-zero")
    }

    fn rect(x: u32, y: u32, w: u32, h: u32) -> PictureRect {
        PictureRect::new(x, y, nz(w), nz(h))
    }

    fn picture(w: u32, h: u32) -> Arc<IndexedPicture> {
        Arc::new(IndexedPicture::new(
            nz(w),
            nz(h),
            PictureEncoding::HdmvRle(Arc::from(vec![1u8, 2, 3])),
        ))
    }

    fn cue(region: PictureRect) -> Result<SubtitlePictureCue, InvalidPictureCue> {
        SubtitlePictureCue::new(
            PictureReferenceSize::new(nz(1920), nz(1080)),
            PicturePosition { x: 10, y: 20 },
            region,
            picture(8, 4),
            Arc::new(PicturePalette::transparent()),
            PictureForcedFlag::Regular,
        )
    }

    #[test]
    fn intersection_returns_overlap_and_none_for_disjoint_or_touching() {
        assert_eq!(
            rect(0, 0, 10, 10).intersection(rect(5, 6, 10, 10)),
            Some(rect(5, 6, 5, 4))
        );
        assert_eq!(rect(0, 0, 10, 10).intersection(rect(10, 0, 5, 5)), None);
        assert_eq!(rect(0, 0, 10, 10).intersection(rect(20, 20, 5, 5)), None);
    }

    #[test]
    fn intersection_does_not_overflow_at_u32_edge() {
        let far = rect(u32::MAX - 1, 0, 5, 1);
        assert_eq!(
            far.intersection(rect(u32::MAX - 1, 0, u32::MAX, 1)),
            Some(rect(u32::MAX - 1, 0, 5, 1))
        );
    }

    #[test]
    fn cue_accepts_region_inside_and_exposes_it() {
        let created = cue(rect(2, 1, 6, 3)).expect("inside");
        assert_eq!(created.visible_region(), rect(2, 1, 6, 3));
        assert_eq!(created.origin(), PicturePosition { x: 10, y: 20 });
        assert_eq!(created.forced(), PictureForcedFlag::Regular);
        assert_eq!(created.picture().encoded_len(), 3);
    }

    #[test]
    fn cue_rejects_region_sticking_out_of_picture() {
        assert!(matches!(
            cue(rect(2, 1, 7, 3)),
            Err(InvalidPictureCue::VisibleRegionOutsidePicture { .. })
        ));
        assert!(cue(rect(0, 0, 8, 5)).is_err());
        assert!(cue(rect(0, 0, 8, 4)).is_ok());
    }

    #[test]
    fn palette_color_lookup_by_index() {
        let mut entries = [PaletteColor::TRANSPARENT; PicturePalette::LEN];
        entries[255] = PaletteColor {
            red: 1,
            green: 2,
            blue: 3,
            alpha: 4,
        };
        let palette = PicturePalette::new(entries);
        assert_eq!(palette.color(255).blue, 3);
        assert_eq!(palette.color(0), PaletteColor::TRANSPARENT);
        assert_eq!(PicturePalette::BYTE_LEN, 1024);
    }
}
