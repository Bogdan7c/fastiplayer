//! Приём событий потока-рисовальщика презентатором: картинки, отчёты, изменения дорожек.
//!
//! Принятая картинка попадает в очередь, только если она нужна: нарисована для текущей
//! дорожки, холста и ревизии входов и НЕ до изменения дорожки, задевающего её срок годности
//! (номер изменения на картинке сравнивается с недавними изменениями, см. `mirror_sync`).

use subtitle_raster::CueSourceId;

use super::super::render_worker::commands::WorkerEvent;
use super::super::render_worker::prepared_queue::{OverlayIdentity, PreparedOverlay};
use super::super::render_worker::validity::{TimeSpan, ValidityInterval};
use super::presenter::SubtitlePresenter;

/// Сколько недавних изменений дорожек помнит UI. Картинка «в пути» отстаёт от текущего
/// состояния на считаные изменения; при переполнении самые старые забываются.
pub(super) const MAX_REMEMBERED_INVALIDATIONS: usize = 64;

/// Изменение зеркала дорожки, которое делает недействительными ранее нарисованные картинки.
#[derive(Debug, Clone, Copy)]
pub(super) struct TrackInvalidation {
    pub(super) serial: u64,
    pub(super) source: CueSourceId,
    pub(super) span: TimeSpan,
}

impl SubtitlePresenter {
    /// Выбрасывает из очереди картинки, задетые изменением дорожки, и запоминает изменение:
    /// картинки, нарисованные потоком ДО него, но ещё не принятые, будут отброшены при приёме.
    pub(super) fn note_track_invalidation(&mut self, invalidation: TrackInvalidation) {
        self.queue
            .invalidate_overlapping(invalidation.source, invalidation.span);
        if self.invalidations.len() >= MAX_REMEMBERED_INVALIDATIONS {
            self.invalidations.pop_front();
        }
        self.invalidations.push_back(invalidation);
    }

    /// Нарисована ли картинка до изменения дорожки, которое задевает её срок годности.
    fn picture_predates_invalidation(
        &self,
        identity: OverlayIdentity,
        interval: ValidityInterval,
        mirror_serial: u64,
    ) -> bool {
        self.invalidations.iter().any(|invalidation| {
            invalidation.source == identity.source
                && invalidation.serial > mirror_serial
                && interval.overlaps(invalidation.span)
        })
    }

    /// Принимает события потока. `true` — пришла картинка, покрывающая время текущего кадра.
    pub(super) fn ingest_events(&mut self) -> bool {
        let mut events = self.link.take_events();
        let current_time = self.last_frame.map(|frame| frame.lookup_time);
        let mut changes_current_frame = false;
        for event in events.drain(..) {
            match event {
                WorkerEvent::Prepared {
                    picture,
                    report,
                    mirror_serial,
                } => {
                    self.stats.rasterizer_calls += 1;
                    self.stats.last_rasterization = report.elapsed;
                    self.stats.slowest_rasterization =
                        self.stats.slowest_rasterization.max(report.elapsed);
                    self.stats.skipped_unsupported_cues += report.skipped_unsupported_cues;
                    self.stats.last_text_language = report.text_language;
                    tracing::debug!(
                        elapsed_us = report.elapsed.as_micros(),
                        cues = report.cue_count,
                        active_events = picture.rendered_events().len(),
                        "Subtitle cues rasterized"
                    );
                    if !self.picture_is_expected(&picture, mirror_serial) {
                        // Нарисовано для входов или дорожки, которые уже сменились.
                        self.stats.dropped_outdated_pictures += 1;
                        continue;
                    }
                    if current_time.is_some_and(|time| picture.interval().contains(time)) {
                        changes_current_frame = true;
                    }
                    self.queue.push(picture);
                }
                WorkerEvent::AttachedFontsInstalled(report) => {
                    self.log_attached_fonts_installed(&report);
                    #[cfg(test)]
                    {
                        self.test_pump.last_installed = Some(report);
                    }
                }
                WorkerEvent::AttachedFontsCleared(outcome) => {
                    tracing::debug!(?outcome, "Attached subtitle fonts cleared");
                    #[cfg(test)]
                    {
                        self.test_pump.last_cleared = Some(outcome);
                    }
                }
                #[cfg(test)]
                WorkerEvent::Idle { token } => self.test_pump.idle_token = Some(token),
                #[cfg(test)]
                WorkerEvent::Diagnostics(diagnostics) => {
                    self.test_pump.last_diagnostics = Some(*diagnostics);
                }
            }
        }
        self.link.recycle_events(events);
        changes_current_frame
    }

    /// Нужна ли UI картинка, пришедшая от потока: та же дорожка и холст, актуальная ревизия
    /// входов и не нарисована до изменения дорожки, задевающего её срок.
    fn picture_is_expected(&self, picture: &PreparedOverlay, mirror_serial: u64) -> bool {
        let identity = picture.identity();
        let Some(target) = self.expected_target else {
            return false;
        };
        identity.source == target.source
            && identity.canvas == target.canvas
            && identity.inputs == self.inputs_revision
            && !self.picture_predates_invalidation(identity, picture.interval(), mirror_serial)
    }

    /// Пишет в лог итог установки вложенных шрифтов (причины типизированы, имён нет).
    fn log_attached_fonts_installed(&self, report: &subtitle_raster::AttachedFontsInstallReport) {
        if !report.rejected.is_empty() {
            tracing::warn!(
                rejected = report.rejected.len(),
                registered = report.registered,
                "Some attached subtitle fonts were rejected"
            );
        }
        tracing::debug!(
            registered = report.registered,
            already_registered = report.already_registered,
            "Attached subtitle fonts installed"
        );
    }
}
