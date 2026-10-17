//! Окно в разделяемой маске: картинка может показывать лишь часть чужого буфера.
//!
//! libass отдаёт `ASS_Image.bitmap` указателем внутрь буфера кэша (со смещением и шагом
//! исходной маски), поэтому обрезка по краю кадра ничего не копирует, а `detect_change` по
//! указателю видит ту же картинку. Здесь то же самое: `SubtitleBitmap` хранит общий
//! `Arc<[u8]>` целиком и смещение первого видимого байта; шаг строки остаётся шагом исходной
//! маски. Потребители читают окно через [`SubtitleBitmap::coverage`] и не знают о смещении.

use std::sync::Arc;

use subtitle_core::SubtitleColor;

use super::SubtitleBitmapKind;
use super::{OverlayPlacementGroup, PixelPoint, SubtitleBitmap, SubtitleBitmapError};

/// Сколько байт нужно маске `width x height` с шагом `stride`: последняя строка без хвоста
/// шага, то есть `(height - 1) * stride + width`. `None` при переполнении.
pub(super) fn required_coverage_len(width: u32, height: u32, stride: usize) -> Option<usize> {
    let rows_before_last = (height as usize).checked_sub(1)?;
    rows_before_last
        .checked_mul(stride)?
        .checked_add(width as usize)
}

impl SubtitleBitmap {
    /// Строит картинку, показывающую окно общего буфера `coverage`: первый видимый байт лежит
    /// по смещению `coverage_offset`, шаг строки - `stride`.
    ///
    /// Проверяет раскладку: непустая площадь, шаг не меньше ширины, а окно
    /// (`coverage_offset + (height - 1) * stride + width`) целиком помещается в буфер.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_shared_view(
        origin: PixelPoint,
        width: u32,
        height: u32,
        stride: usize,
        coverage: Arc<[u8]>,
        coverage_offset: usize,
        color: SubtitleColor,
        kind: SubtitleBitmapKind,
    ) -> Result<Self, SubtitleBitmapError> {
        if width == 0 || height == 0 {
            return Err(SubtitleBitmapError::EmptyArea { width, height });
        }
        if stride < width as usize {
            return Err(SubtitleBitmapError::StrideSmallerThanWidth { stride, width });
        }
        let window_len = required_coverage_len(width, height, stride).unwrap_or(usize::MAX);
        let required = window_len.saturating_add(coverage_offset);
        if coverage.len() < required {
            return Err(SubtitleBitmapError::CoverageTooShort {
                actual: coverage.len(),
                required,
            });
        }
        Ok(Self {
            origin,
            width,
            height,
            stride,
            coverage,
            coverage_offset,
            color,
            kind,
            // Группа по умолчанию - основная; ASS-растеризатор переопределяет её через
            // `with_placement_group`.
            placement_group: OverlayPlacementGroup::Main,
            // Владельца проставляет растеризатор, когда ключ реплики уже известен.
            owner: None,
        })
    }

    /// Окно маски: ровно байты от первого видимого до конца последней видимой строки.
    pub(super) fn coverage_window(&self) -> &[u8] {
        // Инвариант `from_shared_view`: окно помещается в буфер; пустой срез - защита от
        // нарушения инварианта, а не штатный путь.
        required_coverage_len(self.width, self.height, self.stride)
            .and_then(|len| {
                let end = self.coverage_offset.checked_add(len)?;
                self.coverage.get(self.coverage_offset..end)
            })
            .unwrap_or(&[])
    }

    /// Показывают ли две картинки одни и те же пиксели одного буфера (тот же `Arc`, то же
    /// смещение окна, ширина, высота и шаг). Это аналог сравнения `bitmap`-указателей и
    /// размеров в `ass_image_compare` libass; содержимое не сравнивается.
    pub fn shows_same_pixels_as(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.stride == other.stride
            && self.coverage_offset == other.coverage_offset
            && Arc::ptr_eq(&self.coverage, &other.coverage)
    }
}
