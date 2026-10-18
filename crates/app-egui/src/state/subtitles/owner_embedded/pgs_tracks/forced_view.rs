//! Вид PGS-дорожки «только forced-картинки»: вторая шкала с теми же событиями, но только с
//! объектами, помеченными автором как forced.
//!
//! Зачем отдельная шкала, а не фильтр в потоке-рисовальщике: активные реплики и «есть ли что
//! показывать» презентатор считает по шкале, которую ему отдали, и по ним же выбирает запасную
//! картинку очереди. Фильтр только в потоке разошёлся бы с UI. Поэтому режим «forced-объекты»
//! (решение владельца, сессия 11, как у проигрывателей Blu-ray: при выключенных субтитрах и
//! без forced-дорожки) показывает шкалу, в которой остальных объектов просто нет.
//!
//! # Инварианты
//! - Шкала вида ведётся ТЕМИ ЖЕ операциями, что и полная (дописать в хвост, укоротить
//!   предшественника, пересобрать), поэтому описывает ровно forced-подмножество полной шкалы.
//! - У вида собственный `CueSourceId`: кэш растеризатора не спутает его реплики с полной шкалой
//!   (одинаковые id событий в разных шкалах).
//! - Композиция без forced-объектов лежит в карте с пустым списком: укорачивать у неё нечего,
//!   но предшественник «известен» (иначе это было бы рассогласованием).

use std::collections::HashMap;

use subtitle_core::{
    PictureForcedFlag, SubtitleContent, SubtitleEvent, SubtitleEventId, SubtitleLanguage,
    SubtitleSyncOffset, SubtitleTimeline,
};
use subtitle_pgs::{CompositionHandle, TailAppend};
use subtitle_raster::CueSourceId;

use crate::state::subtitles::active_track::ActiveSubtitleTrack;

/// Forced-подмножество шкалы одной PGS-дорожки.
pub(in crate::state::subtitles) struct ForcedPicturesView {
    /// Шкала вида: её отдают презентатору, когда показываются только forced-картинки.
    track: ActiveSubtitleTrack,
    /// События вида по композициям (для укорочения предшественника).
    timeline_events: HashMap<CompositionHandle, Vec<SubtitleEventId>>,
}

impl ForcedPicturesView {
    /// Пустой вид с собственной идентичностью источника.
    pub(in crate::state::subtitles) fn new(
        source_id: CueSourceId,
        sync_offset: SubtitleSyncOffset,
        language: Option<SubtitleLanguage>,
    ) -> Self {
        Self {
            track: ActiveSubtitleTrack::new(
                source_id,
                SubtitleTimeline::new(),
                sync_offset,
                language,
            ),
            timeline_events: HashMap::new(),
        }
    }

    /// Шкала вида (только чтение).
    pub(super) fn track(&self) -> &ActiveSubtitleTrack {
        &self.track
    }

    /// Общий сдвиг синхронизации media: вид показывается в том же времени, что полная шкала.
    pub(super) fn set_sync_offset(&mut self, offset: SubtitleSyncOffset) {
        self.track.set_sync_offset(offset);
    }

    /// Повторяет на виде дописывание композиции в хвост. `false` - вид не принял изменение
    /// (предшественник неизвестен или не укорачивается): вызывающий пересоберёт всё из
    /// хранилища.
    pub(super) fn append(&mut self, tail: &TailAppend) -> bool {
        if let Some(closed) = tail.predecessor_closed {
            let Some(predecessor_events) = self.timeline_events.get(&closed.composition) else {
                return false;
            };
            for &event_id in predecessor_events {
                if self
                    .track
                    .shorten_event_end(event_id, closed.new_end)
                    .is_err()
                {
                    return false;
                }
            }
        }
        let (_report, event_ids) = self
            .track
            .append_events_with_ids(forced_pictures_only(&tail.events));
        self.timeline_events.insert(tail.composition, event_ids);
        true
    }

    /// Пересоздаёт вид из композиций хранилища под новым источником.
    pub(super) fn rebuild(
        &mut self,
        source_id: CueSourceId,
        compositions: &[(CompositionHandle, Vec<SubtitleEvent>)],
    ) {
        self.track.restart_with_source(source_id);
        self.timeline_events.clear();
        for (handle, events) in compositions {
            let (_report, event_ids) = self
                .track
                .append_events_with_ids(forced_pictures_only(events));
            self.timeline_events.insert(*handle, event_ids);
        }
    }
}

/// События с forced-картинками (картинки делят пиксели через `Arc`, копия дешёвая).
fn forced_pictures_only(events: &[SubtitleEvent]) -> Vec<SubtitleEvent> {
    events
        .iter()
        .filter(|event| {
            matches!(
                event.content(),
                SubtitleContent::Picture(cue) if cue.forced() == PictureForcedFlag::Forced
            )
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::Arc;

    use subtitle_core::{
        IndexedPicture, PictureEncoding, PicturePalette, PicturePosition, PictureRect,
        PictureReferenceSize, SubtitleLayer, SubtitlePictureCue, SubtitleTime,
    };

    use super::*;

    fn nonzero(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("test size is non-zero")
    }

    /// Событие-картинка `[start_s, end_s)` секунд с заданным forced-флагом.
    fn picture_event(start_s: i64, end_s: i64, forced: PictureForcedFlag) -> SubtitleEvent {
        let picture = Arc::new(IndexedPicture::new(
            nonzero(4),
            nonzero(4),
            PictureEncoding::HdmvRle(Arc::from(vec![0_u8; 4])),
        ));
        let cue = SubtitlePictureCue::new(
            PictureReferenceSize::new(nonzero(64), nonzero(64)),
            PicturePosition { x: 0, y: 0 },
            PictureRect::new(0, 0, nonzero(4), nonzero(4)),
            picture,
            Arc::new(PicturePalette::transparent()),
            forced,
        )
        .expect("region is inside the picture");
        SubtitleEvent::new(
            SubtitleTime::from_micros(start_s * 1_000_000),
            SubtitleTime::from_micros(end_s * 1_000_000),
            SubtitleLayer::DEFAULT,
            SubtitleContent::Picture(cue),
        )
        .expect("end is after start")
    }

    #[test]
    fn only_forced_pictures_survive_the_filter() {
        let events = [
            picture_event(1, 2, PictureForcedFlag::Regular),
            picture_event(1, 2, PictureForcedFlag::Forced),
        ];
        let kept = forced_pictures_only(&events);
        assert_eq!(kept, vec![events[1].clone()]);
    }
}
