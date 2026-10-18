//! Зеркала дорожек на стороне потока-рисовальщика.
//!
//! Поток не может заглянуть в таймлайны UI (они живут в `MediaSubtitles`), поэтому держит свои
//! копии: `source_id -> дорожка`. Копии меняются только командами [`TrackCommand`] из канала, так
//! что порядок изменений тот же, что у UI, и `SubtitleEventId` в зеркале совпадают с id у UI:
//! id = порядок вставки, а дописывание идёт теми же пачками в том же порядке.

use std::collections::HashMap;

use subtitle_core::{ShortenOutcome, SubtitleLanguage, SubtitleTimeline};
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

/// Что изменение зеркала сделало с записью потока о выданном.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct MirrorEffect {
    /// Выданное в этом отрезке недействительно целиком (дописаны реплики, замена дорожки).
    pub(super) invalidated: Option<TimeSpan>,
    /// Отрезок, который потеряли укороченные события: выданное до его начала остаётся верным
    /// (срок обрезается), выданное внутри — выбрасывается (`ValidityInterval::after_shortening`).
    pub(super) shortened: Option<TimeSpan>,
}

/// Все зеркала потока.
#[derive(Default)]
pub(super) struct TrackMirrors {
    tracks: HashMap<CueSourceId, MirroredTrack>,
}

impl TrackMirrors {
    /// Применяет команду и сообщает, чьи записи о выданном стали неверны: для `Update` -
    /// время дописанных реплик и потерянные отрезки укороченных, для `Replace` уже известной
    /// дорожки - всё время.
    pub(super) fn apply(&mut self, command: TrackCommand) -> MirrorEffect {
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
                MirrorEffect {
                    invalidated: replaced.map(|_| TimeSpan::everything()),
                    shortened: None,
                }
            }
            TrackCommand::Update {
                serial,
                source,
                shortenings,
                appended,
            } => {
                let Some(track) = self.tracks.get_mut(&source) else {
                    return MirrorEffect::default();
                };
                let mut effect = MirrorEffect::default();
                for shortening in shortenings {
                    match track
                        .timeline
                        .shorten_event_end(shortening.id, shortening.new_end)
                    {
                        Ok(ShortenOutcome::Shortened {
                            previous_end,
                            new_end,
                        }) => {
                            let lost = TimeSpan::new(new_end, previous_end);
                            effect.shortened =
                                Some(effect.shortened.map_or(lost, |span| span.union(lost)));
                        }
                        Ok(ShortenOutcome::AlreadyEndsThere) => {}
                        Err(error) => {
                            // Зеркало разошлось с UI (id или конец не те): доверять ему нельзя,
                            // выданное считаем недействительным целиком.
                            tracing::warn!(
                                %error,
                                "Subtitle mirror rejected an end shortening; invalidating the track"
                            );
                            effect.invalidated = Some(TimeSpan::everything());
                        }
                    }
                }
                if let Some(appended_span) = appended
                    .iter()
                    .map(|event| TimeSpan::new(event.start(), event.end()))
                    .reduce(TimeSpan::union)
                {
                    let expected_len = track.timeline.len().saturating_add(appended.len());
                    track.timeline.extend(appended);
                    if track.timeline.len() != expected_len {
                        // Дедупликация зеркала разошлась с UI: id событий сдвинулись.
                        tracing::warn!(
                            expected_len,
                            actual_len = track.timeline.len(),
                            "Subtitle mirror deduplicated appended events differently from the UI"
                        );
                    }
                    effect.invalidated = Some(
                        effect
                            .invalidated
                            .map_or(appended_span, |span| span.union(appended_span)),
                    );
                }
                track.serial = serial;
                effect
            }
            TrackCommand::Forget { source } => {
                self.tracks.remove(&source);
                MirrorEffect::default()
            }
        }
    }

    /// Зеркало дорожки, если оно есть.
    pub(super) fn get(&self, source: CueSourceId) -> Option<&MirroredTrack> {
        self.tracks.get(&source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use subtitle_core::{
        CuePlacement, EndShortening, SpanStyle, StyledText, SubtitleContent, SubtitleEvent,
        SubtitleEventId, SubtitleLayer, SubtitleTextCue, SubtitleTime, TextSpan,
    };

    fn ms(value: i64) -> SubtitleTime {
        SubtitleTime::from_millis(value)
    }

    fn event(start_ms: i64, end_ms: i64) -> SubtitleEvent {
        SubtitleEvent::new(
            ms(start_ms),
            ms(end_ms),
            SubtitleLayer::DEFAULT,
            SubtitleContent::Text(SubtitleTextCue {
                text: StyledText::new(vec![TextSpan::text("x", SpanStyle::default())]),
                placement: CuePlacement::default(),
            }),
        )
        .expect("interval")
    }

    fn mirrors_with_open_event(source: CueSourceId) -> (TrackMirrors, SubtitleEventId) {
        let (timeline, _) = SubtitleTimeline::from_events([event(0, 1_000)]);
        let id = timeline.iter().next().map(|(id, _)| id).expect("one event");
        let mut mirrors = TrackMirrors::default();
        mirrors.apply(TrackCommand::Replace {
            serial: 1,
            source,
            language: None,
            timeline,
        });
        (mirrors, id)
    }

    #[test]
    fn update_applies_the_shortening_and_reports_the_lost_span() {
        let source = CueSourceId::new(1);
        let (mut mirrors, id) = mirrors_with_open_event(source);

        let effect = mirrors.apply(TrackCommand::Update {
            serial: 2,
            source,
            shortenings: vec![EndShortening {
                id,
                previous_end: ms(1_000),
                new_end: ms(400),
            }],
            appended: Vec::new(),
        });

        assert_eq!(effect.shortened, Some(TimeSpan::new(ms(400), ms(1_000))));
        assert_eq!(effect.invalidated, None);
        let mirrored = mirrors.get(source).expect("mirror exists");
        assert_eq!(mirrored.serial, 2);
        assert_eq!(
            mirrored.timeline.event(id).map(SubtitleEvent::end),
            Some(ms(400))
        );
    }

    #[test]
    fn update_for_an_unknown_source_changes_nothing() {
        let (mut mirrors, id) = mirrors_with_open_event(CueSourceId::new(1));
        let effect = mirrors.apply(TrackCommand::Update {
            serial: 2,
            source: CueSourceId::new(99),
            shortenings: vec![EndShortening {
                id,
                previous_end: ms(1_000),
                new_end: ms(400),
            }],
            appended: Vec::new(),
        });
        assert_eq!(effect, MirrorEffect::default());
    }

    #[test]
    fn a_shortening_the_mirror_cannot_apply_invalidates_the_whole_track() {
        let source = CueSourceId::new(1);
        let (mut mirrors, id) = mirrors_with_open_event(source);
        // Удлинение недопустимо: зеркало разошлось с UI.
        let effect = mirrors.apply(TrackCommand::Update {
            serial: 2,
            source,
            shortenings: vec![EndShortening {
                id,
                previous_end: ms(1_000),
                new_end: ms(5_000),
            }],
            appended: Vec::new(),
        });
        assert_eq!(effect.invalidated, Some(TimeSpan::everything()));
        assert_eq!(
            mirrors
                .get(source)
                .and_then(|m| m.timeline.event(id))
                .map(SubtitleEvent::end),
            Some(ms(1_000)),
            "зеркало не тронуто"
        );
    }
}
