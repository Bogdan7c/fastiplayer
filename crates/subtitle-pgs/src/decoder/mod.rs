//! Автомат дорожки PGS: поток блоков -> композиции с растровыми репликами.
//!
//! Блок Matroska содержит сегменты; набор отображения (display set) идёт от PCS до END.
//! Автомат собирает набор и применяет его целиком по END. Состояние эпохи (окна, палитры,
//! объекты) переживает наборы; начало эпохи его сбрасывает.
//!
//! Устойчивость: битый сегмент, неизвестный тип, потерянный фрагмент — пропуск со счётчиком
//! ([`PgsDecodeStatistics`]); дорожка никогда не прерывается и не паникует.

mod compose;
mod epoch;
mod objects;
mod stats;

use std::sync::Arc;

use subtitle_core::{
    IndexedPicture, PictureEncoding, PicturePalette, PictureReferenceSize, SubtitlePictureCue,
    SubtitleTime,
};

use self::epoch::EpochState;
use self::objects::{AssembledObject, ObjectAssembler};
pub use self::stats::PgsDecodeStatistics;
use crate::color::PgsColorMatrix;
use crate::error::SegmentParseError;
use crate::ods::parse_ods;
use crate::pcs::{CompositionState, PresentationComposition, parse_pcs};
use crate::pds::parse_pds;
use crate::segment::{Segment, SegmentKind, split_block};
use crate::wds::{WindowDefinition, parse_wds};

/// Лимиты памяти декодера; значений по умолчанию нет — задаёт владелец конфигурации.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PgsDecoderLimits {
    /// Наибольший размер RLE одного объекта.
    pub max_object_rle_bytes: usize,
    /// Наибольший суммарный размер RLE всех объектов эпохи.
    pub max_epoch_rle_bytes: usize,
}

/// Результат применения одного набора отображения.
#[derive(Debug, Clone, PartialEq)]
pub struct PgsComposition {
    /// Момент начала показа: время блока, в котором пришёл PCS набора.
    pub start: SubtitleTime,
    /// Реплики по порядку объектов композиции. Пусто — «очистка» (ничего не показывать).
    pub cues: Vec<SubtitlePictureCue>,
}

/// Принимать ли набор или игнорировать его целиком.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SetDisposition {
    Apply,
    IgnoreUntilEntryPoint,
}

/// Набор отображения в процессе сборки.
struct PendingSet {
    start: SubtitleTime,
    pcs: PresentationComposition,
    disposition: SetDisposition,
    windows: Vec<WindowDefinition>,
    palettes: Vec<(u8, PicturePalette)>,
    objects: Vec<AssembledObject>,
}

/// Декодер одной дорожки PGS. Кормится блоками строго в порядке поступления.
pub struct PgsTrackDecoder {
    limits: PgsDecoderLimits,
    stats: PgsDecodeStatistics,
    epoch: EpochState,
    assembler: ObjectAssembler,
    pending: Option<PendingSet>,
    /// `true`, пока не встретился Epoch Start / Acquisition Point (начало потока или seek).
    awaiting_entry_point: bool,
}

impl PgsTrackDecoder {
    pub fn new(limits: PgsDecoderLimits) -> Self {
        Self {
            limits,
            stats: PgsDecodeStatistics::default(),
            epoch: EpochState::default(),
            assembler: ObjectAssembler::default(),
            pending: None,
            awaiting_entry_point: true,
        }
    }

    pub fn statistics(&self) -> &PgsDecodeStatistics {
        &self.stats
    }

    /// Разрыв потока (seek): недособранный набор и состояние эпохи отбрасываются; наборы
    /// Normal игнорируются, пока не придёт Epoch Start или Acquisition Point.
    pub fn reset_after_discontinuity(&mut self) {
        self.pending = None;
        self.assembler.discard_unfinished();
        self.epoch.clear();
        self.awaiting_entry_point = true;
        self.stats.discontinuities += 1;
    }

    /// Обрабатывает payload одного блока; `block_time` — время блока в контейнере.
    /// Возвращает композиции наборов, завершившихся в этом блоке (обычно 0 или 1).
    pub fn push_block(&mut self, block_time: SubtitleTime, payload: &[u8]) -> Vec<PgsComposition> {
        let split = split_block(payload);
        if split.framing_error.is_some() {
            self.stats.blocks_with_framing_errors += 1;
        }
        let mut finished = Vec::new();
        for segment in &split.segments {
            if let Some(composition) = self.handle_segment(block_time, *segment) {
                finished.push(composition);
            }
        }
        finished
    }

    fn handle_segment(
        &mut self,
        block_time: SubtitleTime,
        segment: Segment<'_>,
    ) -> Option<PgsComposition> {
        match segment.kind {
            SegmentKind::PresentationComposition => self.handle_pcs(block_time, segment.payload),
            SegmentKind::EndOfDisplaySet => self.handle_end(),
            SegmentKind::Unknown(_) => {
                self.stats.unknown_segments += 1;
                None
            }
            SegmentKind::WindowDefinition
            | SegmentKind::PaletteDefinition
            | SegmentKind::ObjectDefinition => {
                self.handle_resource(segment);
                None
            }
        }
    }

    /// PCS открывает набор; незакрытый предыдущий закрывается неявно.
    fn handle_pcs(&mut self, block_time: SubtitleTime, payload: &[u8]) -> Option<PgsComposition> {
        let implicit = if self.pending.is_some() {
            self.stats.implicit_end_sets += 1;
            self.handle_end()
        } else {
            None
        };
        match parse_pcs(payload) {
            Ok(pcs) => {
                let disposition = self.classify(&pcs);
                self.pending = Some(PendingSet {
                    start: block_time,
                    pcs,
                    disposition,
                    windows: Vec::new(),
                    palettes: Vec::new(),
                    objects: Vec::new(),
                });
            }
            Err(SegmentParseError::OversizedVideoSize { .. }) => {
                self.stats.oversized_reference_frames += 1;
            }
            Err(_) => self.stats.malformed_segments += 1,
        }
        implicit
    }

    /// Решает, принимать ли набор, учитывая точку входа.
    fn classify(&mut self, pcs: &PresentationComposition) -> SetDisposition {
        match pcs.state {
            CompositionState::EpochStart | CompositionState::AcquisitionPoint => {
                SetDisposition::Apply
            }
            CompositionState::Normal | CompositionState::Unknown(_) => {
                if matches!(pcs.state, CompositionState::Unknown(_)) {
                    self.stats.unknown_composition_states += 1;
                }
                if self.awaiting_entry_point {
                    self.stats.sets_ignored_until_entry_point += 1;
                    SetDisposition::IgnoreUntilEntryPoint
                } else {
                    SetDisposition::Apply
                }
            }
        }
    }

    /// WDS / PDS / ODS: складываются в набор (или пропускаются, если набора нет или он игнорируется).
    fn handle_resource(&mut self, segment: Segment<'_>) {
        let Some(pending) = self.pending.as_mut() else {
            self.stats.orphan_segments += 1;
            return;
        };
        if pending.disposition == SetDisposition::IgnoreUntilEntryPoint {
            return;
        }
        match segment.kind {
            SegmentKind::WindowDefinition => match parse_wds(segment.payload) {
                Ok(windows) => pending.windows.extend(windows),
                Err(_) => self.stats.malformed_segments += 1,
            },
            SegmentKind::PaletteDefinition => match parse_pds(segment.payload) {
                Ok(definition) => {
                    if definition.trailing_partial_bytes > 0 {
                        self.stats.palettes_with_partial_trailing_entry += 1;
                    }
                    // Матрица палитры зависит от формата видео из PCS этого набора.
                    let matrix = PgsColorMatrix::for_reference_height(
                        pending.pcs.reference_size.height().get(),
                    );
                    pending
                        .palettes
                        .push((definition.palette_id, definition.to_palette(matrix)));
                }
                Err(_) => self.stats.malformed_segments += 1,
            },
            SegmentKind::ObjectDefinition => match parse_ods(segment.payload) {
                Ok(fragment) => {
                    if let Some(object) =
                        self.assembler.feed(&fragment, self.limits, &mut self.stats)
                    {
                        pending.objects.push(object);
                    }
                }
                Err(_) => self.stats.malformed_segments += 1,
            },
            _ => {}
        }
    }

    /// END: применяет собранный набор.
    fn handle_end(&mut self) -> Option<PgsComposition> {
        let Some(pending) = self.pending.take() else {
            self.stats.orphan_segments += 1;
            return None;
        };
        let unfinished = self.assembler.discard_unfinished();
        if pending.disposition == SetDisposition::IgnoreUntilEntryPoint {
            return None;
        }
        self.stats.incomplete_objects += unfinished as u64;
        self.apply_set(pending)
    }

    fn apply_set(&mut self, set: PendingSet) -> Option<PgsComposition> {
        let PendingSet {
            start,
            pcs,
            windows,
            palettes,
            objects,
            ..
        } = set;
        if pcs.state == CompositionState::EpochStart {
            self.epoch.clear();
        }
        // Точка входа пройдена: дальше Normal-наборы опираются на состояние эпохи.
        if matches!(
            pcs.state,
            CompositionState::EpochStart | CompositionState::AcquisitionPoint
        ) {
            self.awaiting_entry_point = false;
        }
        for window in windows {
            self.epoch.windows.insert(window.window_id, window);
        }
        for (palette_id, palette) in palettes {
            self.epoch.palettes.insert(palette_id, Arc::new(palette));
        }
        for object in objects {
            self.store_object(object, pcs.reference_size);
        }
        self.stats.display_sets_applied += 1;
        // Несобираемая композиция (нет объекта или палитры) всё равно сменила картинку на
        // экране автора: прошлая композиция в этот момент заведомо закончилась. Поэтому
        // вместо пропуска выдаём «очистку» — лучше ничего не показать, чем оставить
        // устаревший субтитр висеть до следующего набора.
        let cues = compose::compose(&pcs, &mut self.epoch, &mut self.stats).unwrap_or_default();
        self.stats.compositions_emitted += 1;
        Some(PgsComposition { start, cues })
    }

    /// Сохраняет объект эпохи. Объект больше кадра композиции невозможен в корректном
    /// потоке (картинка не поместилась бы на экран) и отвергается со счётчиком: так память
    /// под индексы ограничена форматом (кадр не больше `MAX_REFERENCE_*`), а не заявкой потока.
    fn store_object(&mut self, object: AssembledObject, reference_size: PictureReferenceSize) {
        if u32::from(object.width) > reference_size.width().get()
            || u32::from(object.height) > reference_size.height().get()
        {
            self.stats.objects_larger_than_reference_frame += 1;
            return;
        }
        let (Some(width), Some(height)) = (
            std::num::NonZeroU32::new(u32::from(object.width)),
            std::num::NonZeroU32::new(u32::from(object.height)),
        ) else {
            // Нулевой размер отсечён разбором ODS; сюда попасть нельзя.
            self.stats.malformed_segments += 1;
            return;
        };
        let picture = Arc::new(IndexedPicture::new(
            width,
            height,
            PictureEncoding::HdmvRle(Arc::from(object.rle_bytes)),
        ));
        if !self
            .epoch
            .try_insert_object(object.object_id, picture, self.limits.max_epoch_rle_bytes)
        {
            self.stats.objects_rejected_over_limit += 1;
        }
    }
}

#[cfg(test)]
mod tests;
