//! PCS (0x16): композиция — какие объекты, где и в каком окне показать.

use std::num::NonZeroU32;

use subtitle_core::{PictureForcedFlag, PictureReferenceSize};

use crate::error::SegmentParseError;
use crate::reader::ByteReader;
use crate::segment::SegmentKind;

/// Состояние композиции.
///
/// Неизвестное значение байта НЕ отбрасывается и трактуется как [`Self::Normal`]:
/// самый безопасный выбор, потому что Normal ничего не сбрасывает и требует уже
/// известных ресурсов (при их отсутствии композиция пропускается со счётчиком).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositionState {
    /// 0x00 — обновление внутри эпохи.
    Normal,
    /// 0x40 — точка входа: набор самодостаточен, с него можно начинать декодирование.
    AcquisitionPoint,
    /// 0x80 — начало эпохи: прежнее состояние (окна, палитры, объекты) сбрасывается.
    EpochStart,
    /// Любое другое значение (сырой байт сохранён для диагностики).
    Unknown(u8),
}

impl CompositionState {
    fn from_byte(byte: u8) -> Self {
        match byte {
            0x00 => Self::Normal,
            0x40 => Self::AcquisitionPoint,
            0x80 => Self::EpochStart,
            other => Self::Unknown(other),
        }
    }

    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Normal => 0x00,
            Self::AcquisitionPoint => 0x40,
            Self::EpochStart => 0x80,
            Self::Unknown(byte) => byte,
        }
    }
}

/// Что обновляет композиция.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteUpdate {
    /// Обычная композиция: список объектов задан полностью.
    FullComposition,
    /// Обновляется только палитра: объекты те же, что в предыдущей композиции.
    PaletteOnly,
}

/// Прямоугольник обрезки внутри картинки объекта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// Объект в композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositionObject {
    pub object_id: u16,
    pub window_id: u8,
    pub forced: PictureForcedFlag,
    /// Позиция на экране: куда попадает левый верхний угол ВИДИМОЙ части
    /// (при обрезке — угол прямоугольника обрезки).
    pub x: u16,
    pub y: u16,
    pub crop: Option<CropRect>,
}

/// Разобранный PCS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationComposition {
    pub reference_size: PictureReferenceSize,
    pub composition_number: u16,
    pub state: CompositionState,
    pub palette_update: PaletteUpdate,
    pub palette_id: u8,
    pub objects: Vec<CompositionObject>,
}

/// Наибольшая ширина кадра композиции, которую принимает разбор PCS.
///
/// ФОРМАТНЫЙ инвариант, а не пользовательская настройка: по спецификации Blu-ray графика
/// PGS живёт в кадре не больше 1920x1080 (у UHD Blu-ray тоже 1920x1080). 4096x2304 —
/// терпимый запас для нестандартных муксеров. Без этой границы враждебный поток мог бы
/// объявить кадр 65535x65535 и заставить декодер выделять сотни мегабайт под ~16 КБ RLE.
pub const MAX_REFERENCE_WIDTH: u16 = 4096;
/// Наибольшая высота кадра композиции (см. [`MAX_REFERENCE_WIDTH`]).
pub const MAX_REFERENCE_HEIGHT: u16 = 2304;

const FLAG_CROPPED: u8 = 0x80;
const FLAG_FORCED: u8 = 0x40;
const PALETTE_UPDATE_FLAG: u8 = 0x80;

/// Разбирает payload PCS. Если бит обрезки выставлен, а байтов под прямоугольник нет —
/// ошибка [`SegmentParseError::Truncated`], а не угадывание.
pub fn parse_pcs(payload: &[u8]) -> Result<PresentationComposition, SegmentParseError> {
    let mut reader = ByteReader::new(SegmentKind::PresentationComposition, payload);
    let raw_width = reader.u16()?;
    let raw_height = reader.u16()?;
    let (Some(width), Some(height)) = (
        NonZeroU32::new(u32::from(raw_width)),
        NonZeroU32::new(u32::from(raw_height)),
    ) else {
        return Err(SegmentParseError::ZeroVideoSize);
    };
    // Размеры больше формата отвергаем сразу: от кадра зависят границы объектов и палитра.
    if width.get() > u32::from(MAX_REFERENCE_WIDTH)
        || height.get() > u32::from(MAX_REFERENCE_HEIGHT)
    {
        return Err(SegmentParseError::OversizedVideoSize {
            width: raw_width,
            height: raw_height,
        });
    }
    let _frame_rate = reader.u8()?; // Частота кадров на показ не влияет.
    let composition_number = reader.u16()?;
    let state = CompositionState::from_byte(reader.u8()?);
    let palette_update = if reader.u8()? == PALETTE_UPDATE_FLAG {
        PaletteUpdate::PaletteOnly
    } else {
        PaletteUpdate::FullComposition
    };
    let palette_id = reader.u8()?;
    let object_count = reader.u8()?;
    let mut objects = Vec::with_capacity(usize::from(object_count));
    for _ in 0..object_count {
        objects.push(parse_object(&mut reader)?);
    }
    Ok(PresentationComposition {
        reference_size: PictureReferenceSize::new(width, height),
        composition_number,
        state,
        palette_update,
        palette_id,
        objects,
    })
}

fn parse_object(reader: &mut ByteReader<'_>) -> Result<CompositionObject, SegmentParseError> {
    let object_id = reader.u16()?;
    let window_id = reader.u8()?;
    let flags = reader.u8()?;
    let x = reader.u16()?;
    let y = reader.u16()?;
    // Прямоугольник обрезки следует за позицией ТОЛЬКО при выставленном бите.
    let crop = if flags & FLAG_CROPPED != 0 {
        Some(CropRect {
            x: reader.u16()?,
            y: reader.u16()?,
            width: reader.u16()?,
            height: reader.u16()?,
        })
    } else {
        None
    };
    let forced = if flags & FLAG_FORCED != 0 {
        PictureForcedFlag::Forced
    } else {
        PictureForcedFlag::Regular
    };
    Ok(CompositionObject {
        object_id,
        window_id,
        forced,
        x,
        y,
        crop,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(state: u8, palette_flag: u8, count: u8) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1920u16.to_be_bytes());
        out.extend_from_slice(&1080u16.to_be_bytes());
        out.push(0x10);
        out.extend_from_slice(&7u16.to_be_bytes());
        out.extend_from_slice(&[state, palette_flag, 3, count]);
        out
    }

    #[test]
    fn parses_plain_and_cropped_forced_objects() {
        let mut payload = header(0x80, 0x00, 2);
        // Объект 1: без обрезки, обычный.
        payload.extend_from_slice(&[0, 1, 0, 0x00, 0, 100, 0, 200]);
        // Объект 2: обрезка + forced.
        payload.extend_from_slice(&[0, 2, 1, 0xC0, 0, 10, 0, 20]);
        payload.extend_from_slice(&[0, 3, 0, 4, 0, 50, 0, 60]);
        let pcs = parse_pcs(&payload).expect("valid");
        assert_eq!(pcs.reference_size.width().get(), 1920);
        assert_eq!(pcs.reference_size.height().get(), 1080);
        assert_eq!(pcs.composition_number, 7);
        assert_eq!(pcs.state, CompositionState::EpochStart);
        assert_eq!(pcs.palette_update, PaletteUpdate::FullComposition);
        assert_eq!(pcs.palette_id, 3);
        assert_eq!(pcs.objects.len(), 2);
        assert_eq!(pcs.objects[0].object_id, 1);
        assert_eq!((pcs.objects[0].x, pcs.objects[0].y), (100, 200));
        assert_eq!(pcs.objects[0].crop, None);
        assert_eq!(pcs.objects[0].forced, PictureForcedFlag::Regular);
        assert_eq!(pcs.objects[1].window_id, 1);
        assert_eq!(pcs.objects[1].forced, PictureForcedFlag::Forced);
        assert_eq!(
            pcs.objects[1].crop,
            Some(CropRect {
                x: 3,
                y: 4,
                width: 50,
                height: 60
            })
        );
    }

    #[test]
    fn palette_only_flag_and_state_variants() {
        let pcs = parse_pcs(&header(0x40, 0x80, 0)).expect("valid");
        assert_eq!(pcs.state, CompositionState::AcquisitionPoint);
        assert_eq!(pcs.palette_update, PaletteUpdate::PaletteOnly);
        assert!(pcs.objects.is_empty());
        assert_eq!(
            parse_pcs(&header(0x00, 0, 0)).expect("valid").state,
            CompositionState::Normal
        );
        assert_eq!(
            parse_pcs(&header(0x33, 0, 0)).expect("valid").state,
            CompositionState::Unknown(0x33)
        );
    }

    #[test]
    fn crop_flag_without_crop_bytes_is_an_error() {
        let mut payload = header(0, 0, 1);
        payload.extend_from_slice(&[0, 1, 0, 0x80, 0, 1, 0, 1, 0, 0]);
        assert!(matches!(
            parse_pcs(&payload),
            Err(SegmentParseError::Truncated { .. })
        ));
    }

    #[test]
    fn declared_object_missing_and_zero_video_size_are_errors() {
        assert!(parse_pcs(&header(0, 0, 1)).is_err());
        let mut zero = header(0, 0, 0);
        zero[0] = 0;
        zero[1] = 0;
        assert_eq!(parse_pcs(&zero), Err(SegmentParseError::ZeroVideoSize));
        assert!(parse_pcs(&[0, 1]).is_err());
    }

    #[test]
    fn frame_larger_than_the_format_allows_is_rejected_but_full_hd_is_accepted() {
        let mut oversized = header(0, 0, 0);
        oversized[0..2].copy_from_slice(&(MAX_REFERENCE_WIDTH + 1).to_be_bytes());
        oversized[2..4].copy_from_slice(&1080u16.to_be_bytes());
        assert_eq!(
            parse_pcs(&oversized),
            Err(SegmentParseError::OversizedVideoSize {
                width: MAX_REFERENCE_WIDTH + 1,
                height: 1080
            })
        );
        let mut full_hd = header(0, 0, 0);
        full_hd[0..2].copy_from_slice(&1920u16.to_be_bytes());
        full_hd[2..4].copy_from_slice(&1080u16.to_be_bytes());
        let parsed = parse_pcs(&full_hd).expect("1920x1080 is valid");
        assert_eq!(parsed.reference_size.width().get(), 1920);
    }
}
