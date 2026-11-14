//! Расписание перезагрузки плейлистов эфира (RFC 8216 §6.3.4).
//!
//! У главного плейлиста и отдельного звука свои сроки: изменившийся плейлист
//! перекачивается через target duration, неизменный — через половину, после `ENDLIST`
//! больше не качается. Вынесено из `refresh.rs` в 16d3 без изменения поведения.

use std::time::{Duration, Instant};

use super::open::SelectedLiveResources;
use super::refresh::HlsLiveRefreshAttempt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HlsLiveDueRenditions {
    pub(super) main: bool,
    pub(super) audio: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct HlsLiveRenditionReloadSchedule {
    deadline: Option<Instant>,
}

impl HlsLiveRenditionReloadSchedule {
    fn initial(now: Instant, target_duration: Duration, ended: bool) -> Option<Self> {
        Self::after_reload(now, target_duration, true, ended)
    }

    fn after_reload(
        request_started: Instant,
        target_duration: Duration,
        changed: bool,
        ended: bool,
    ) -> Option<Self> {
        let deadline = if ended {
            None
        } else {
            let delay = if changed {
                target_duration
            } else {
                target_duration / 2
            };
            Some(request_started.checked_add(delay)?)
        };
        Some(Self { deadline })
    }

    fn is_due(self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| deadline <= now)
    }
}

pub(super) struct HlsLiveReloadSchedules {
    main: HlsLiveRenditionReloadSchedule,
    audio: Option<HlsLiveRenditionReloadSchedule>,
}

impl HlsLiveReloadSchedules {
    pub(super) fn initial(now: Instant, resources: &SelectedLiveResources) -> Option<Self> {
        Some(Self {
            main: HlsLiveRenditionReloadSchedule::initial(
                now,
                Duration::from_secs(resources.main_media.target_duration_seconds),
                resources.main_media.end_list,
            )?,
            audio: match resources.audio_media.as_ref() {
                Some(media) => Some(HlsLiveRenditionReloadSchedule::initial(
                    now,
                    Duration::from_secs(media.target_duration_seconds),
                    media.end_list,
                )?),
                None => None,
            },
        })
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.main
            .deadline
            .into_iter()
            .chain(self.audio.and_then(|schedule| schedule.deadline))
            .min()
    }

    pub(super) fn due(&self, now: Instant) -> HlsLiveDueRenditions {
        HlsLiveDueRenditions {
            main: self.main.is_due(now),
            audio: self.audio.is_some_and(|schedule| schedule.is_due(now)),
        }
    }

    /// Делает каждый ещё обновляемый плейлист «пора качать» уже сейчас.
    ///
    /// Плейлист, получивший `ENDLIST`, больше не качается и не воскрешается.
    pub(super) fn make_every_rendition_due(&mut self, now: Instant) {
        for schedule in std::iter::once(&mut self.main).chain(self.audio.as_mut()) {
            if schedule.deadline.is_some() {
                schedule.deadline = Some(now);
            }
        }
    }

    /// Плейлисты, которые сейчас пора качать, но сеть недоступна: переносит их на `retry_at`.
    ///
    /// Плейлисты, которым ещё не пора, не трогаются; после `ENDLIST` срока нет вовсе.
    pub(super) fn postpone_due_until(&mut self, now: Instant, retry_at: Instant) {
        for schedule in std::iter::once(&mut self.main).chain(self.audio.as_mut()) {
            if schedule.is_due(now) {
                schedule.deadline = Some(retry_at);
            }
        }
    }

    pub(super) fn record_reload(
        &mut self,
        request_started: Instant,
        attempt: &HlsLiveRefreshAttempt,
    ) -> bool {
        if attempt.due.main {
            let Some(main) = HlsLiveRenditionReloadSchedule::after_reload(
                request_started,
                Duration::from_secs(attempt.next.main_media.target_duration_seconds),
                attempt.main_changed,
                attempt.next.main_media.end_list,
            ) else {
                return false;
            };
            self.main = main;
        }
        if attempt.due.audio {
            let Some(audio_media) = attempt.next.audio_media.as_ref() else {
                return false;
            };
            let Some(audio) = HlsLiveRenditionReloadSchedule::after_reload(
                request_started,
                Duration::from_secs(audio_media.target_duration_seconds),
                attempt.audio_changed,
                audio_media.end_list,
            ) else {
                return false;
            };
            self.audio = Some(audio);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_rendition_cadence_uses_changed_and_unchanged_delays() {
        let now = Instant::now();
        let main =
            HlsLiveRenditionReloadSchedule::after_reload(now, Duration::from_secs(8), false, false)
                .expect("valid main schedule");
        let audio =
            HlsLiveRenditionReloadSchedule::after_reload(now, Duration::from_secs(10), true, false)
                .expect("valid audio schedule");
        let schedules = HlsLiveReloadSchedules {
            main,
            audio: Some(audio),
        };

        assert_eq!(
            schedules.next_deadline(),
            Some(now + Duration::from_secs(4))
        );
        assert_eq!(
            schedules.due(now + Duration::from_secs(4)),
            HlsLiveDueRenditions {
                main: true,
                audio: false,
            }
        );
        assert_eq!(
            schedules.due(now + Duration::from_secs(10)),
            HlsLiveDueRenditions {
                main: true,
                audio: true,
            }
        );
    }
}
