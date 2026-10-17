//! Зеркала дорожек на стороне потока-рисовальщика.
//!
//! Поток не может заглянуть в таймлайны UI (они живут в `MediaSubtitles`), поэтому держит свои
//! копии: `source_id -> дорожка`. Копии меняются только командами [`TrackCommand`] из канала, так
//! что порядок изменений тот же, что у UI, и `SubtitleEventId` в зеркале совпадают с id у UI:
//! id = порядок вставки, а дописывание идёт теми же пачками в том же порядке.

use std::collections::HashMap;

use subtitle_core::{SubtitleLanguage, SubtitleTimeline};
use subtitle_raster::CueSourceId;

use super::commands::TrackCommand;
use super::validity::TimeSpan;

/// Зеркало одной дорожки.
pub(super) struct MirroredTrack {
    /// Номер последнего применённого изменения (`TrackCommand::*::serial`).
    pub(super) serial: u64,
    pub(super) timeline: SubtitleTimeline,
    pub(super) language: Option<SubtitleLanguage>,
}

/// Все зеркала потока.
#[derive(Default)]
pub(super) struct TrackMirrors {
    tracks: HashMap<CueSourceId, MirroredTrack>,
}

impl TrackMirrors {
    /// Применяет команду. Возвращает отрезок времени, чьи записи о выданном стали неверны:
    /// для `Append` - время дописанных реплик, для `Replace` уже известной дорожки - всё время.
    pub(super) fn apply(&mut self, command: TrackCommand) -> Option<TimeSpan> {
        match command {
            TrackCommand::Replace {
                serial,
                source,
                language,
                timeline,
            } => {
                let replaced = self.tracks.insert(
                    source,
                    MirroredTrack {
                        serial,
                        timeline,
                        language,
                    },
                );
                replaced.map(|_| TimeSpan::everything())
            }
            TrackCommand::Append {
                serial,
                source,
                events,
            } => {
                let track = self.tracks.get_mut(&source)?;
                let span = events
                    .iter()
                    .map(|event| TimeSpan::new(event.start(), event.end()))
                    .reduce(TimeSpan::union)?;
                track.timeline.extend(events);
                track.serial = serial;
                Some(span)
            }
            TrackCommand::Forget { source } => {
                self.tracks.remove(&source);
                None
            }
        }
    }

    /// Зеркало дорожки, если оно есть.
    pub(super) fn get(&self, source: CueSourceId) -> Option<&MirroredTrack> {
        self.tracks.get(&source)
    }
}
