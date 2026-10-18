//! Презентатор субтитров: UI-фасад над потоком-рисовальщиком.
//!
//! # Что делает на потоке UI
//! На каждом отправляемом кадре: считает время поиска реплик, активные реплики, держит зеркало
//! дорожки в актуальном состоянии у потока, сообщает «часы показа» (только при изменении) и
//! выбирает готовую картинку из очереди ([`PreparedQueue`]). Растеризатор здесь НЕ вызывается и
//! поток НЕ ожидается: нет готовой картинки — берётся последняя без закончившихся реплик.
//!
//! # Инварианты
//! - Идентичность картинки: источник реплик + холст + ревизия входов. Ревизия растёт при смене
//!   стиля, настроек ASS (в том числе размера картинки видео), шрифтов и при `forget`.
//! - Картинки, пришедшие не для ожидаемой идентичности, выбрасываются при приёме.
//! - На точном пути выбора нет аллокаций: показанная картинка копируется только при смене.
//! - Отказ потока не паника: показ пропадает, владелец узнаёт причину через `take_failure`.

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::time::Duration;

use render_core::PictureRect;
use subtitle_core::{SubtitleEventId, SubtitleLanguage, SubtitleTime};
use subtitle_raster::{
    AssRenderOptions, AttachedFontLimits, CanvasSize, PreparedAttachedFonts, RasterLimits,
    SubtitleAppearance, SubtitleFontLibrary, SubtitleOverlay,
};

use super::super::active_track::ActiveSubtitleTrack;
use super::super::render_worker::RenderWorkerFailure;
use super::super::render_worker::clock_tracker::{ClockTracker, PlaybackObservation};
use super::super::render_worker::commands::{LookInputs, RenderTarget, WorkerCommand};
use super::super::render_worker::lateness::LatenessMonitor;
use super::super::render_worker::mirror_sync::{MirrorSync, SyncOutcome, TrackRole};
use super::super::render_worker::prepared_queue::{
    OverlayIdentity, PreparedQueue, RenderInputsRevision, Selection,
};
use super::super::render_worker::schedule::{PrerenderFrames, PresentationClock};
use super::super::render_worker::validity::TimeSpan;
use super::super::render_worker::worker::WorkerSettings;
use super::super::user_style::UserSubtitleStyle;
use super::ass_options::AssPresentation;
use super::ingest::{InvalidationKind, TrackInvalidation};
use super::worker_link::WorkerLink;
use super::{PresentationStats, SubmittedFrameForSubtitles};
use crate::app_wake::AppWakePort;

/// Настройки, с которыми создаётся поток-рисовальщик (меняются только до его создания).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct PresenterSettings {
    /// Лимиты растеризатора.
    pub(in crate::state::subtitles) raster_limits: RasterLimits,
    /// На сколько кадров вперёд рисовать (конфиг `prerender_frames`).
    pub(in crate::state::subtitles) prerender_frames: u32,
    /// Сколько готовых картинок держать (конфиг `prepared_overlays`).
    pub(in crate::state::subtitles) prepared_overlays: u32,
}

/// Были ли на кадре активные реплики у проверяемой дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActiveCues {
    /// Реплик нет (или кадр нельзя разместить).
    None,
    /// Реплики есть; показать их удалось или нет - решает поток.
    Present,
}

/// Что показано сейчас (копия готовой картинки очереди).
pub(super) struct ShownOverlay {
    overlay: SubtitleOverlay,
}

impl ShownOverlay {
    /// Та же ли это картинка, что `other` (ревизия и общие маски; без аллокаций).
    fn is_same_picture_as(&self, other: &SubtitleOverlay) -> bool {
        let mine = self.overlay.bitmaps();
        let theirs = other.bitmaps();
        self.overlay.revision() == other.revision()
            && self.overlay.canvas() == other.canvas()
            && mine.len() == theirs.len()
            && mine
                .iter()
                .zip(theirs)
                .all(|(a, b)| a.shows_same_pixels_as(b))
    }
}

/// Куда и когда последний раз отправлены «часы» (чтобы не слать одно и то же).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SentPresentation {
    target: RenderTarget,
    clock: PresentationClock,
}

/// Состояние кадра, нужное, когда кадров нет (seek/scrub со старым кадром).
#[derive(Debug, Clone, Copy)]
pub(super) struct LastFrame {
    pub(super) canvas: CanvasSize,
    pub(super) lookup_time: SubtitleTime,
}

/// UI-фасад потока-рисовальщика и последнего показанного overlay.
pub(in crate::state::subtitles) struct SubtitlePresenter {
    pub(super) link: WorkerLink,
    pub(super) appearance: SubtitleAppearance,
    /// Часть настроек ASS, зависящая от стиля пользователя (см. `ass_options`).
    pub(super) ass: AssPresentation,
    pub(super) settings: PresenterSettings,
    pub(super) system_language: Option<SubtitleLanguage>,
    pub(super) queue: PreparedQueue,
    pub(super) inputs_revision: RenderInputsRevision,
    /// Настройки ASS последнего кадра: их смена (размер картинки видео) меняет ревизию.
    pub(super) last_ass_options: Option<AssRenderOptions>,
    /// Какие входы рисования уже отправлены потоку.
    pub(super) sent_look: Option<(RenderInputsRevision, AssRenderOptions)>,
    pub(super) mirror: MirrorSync,
    pub(super) clock: ClockTracker,
    pub(super) observation: PlaybackObservation,
    pub(super) sent_presentation: Option<SentPresentation>,
    /// Для какой дорожки и холста поток рисует сейчас: чужие картинки выбрасываются при
    /// приёме. Ревизия входов сравнивается с ЖИВОЙ `inputs_revision` (она растёт между
    /// кадрами при смене стиля, а картинки новой ревизии не должны отбрасываться).
    pub(super) expected_target: Option<RenderTarget>,
    /// Недавние замены и дописывания дорожек: по ним отбрасываются картинки, нарисованные
    /// потоком ДО этих изменений (они были в пути, когда UI уже выбросил своё).
    pub(super) invalidations: VecDeque<TrackInvalidation>,
    pub(super) last_frame: Option<LastFrame>,
    /// Переиспользуемый буфер: реплики, активные на текущем кадре.
    pub(super) scratch_active_events: Vec<SubtitleEventId>,
    pub(super) shown: Option<ShownOverlay>,
    pub(super) stats: PresentationStats,
    pub(super) lateness: LatenessMonitor,
    #[cfg(test)]
    pub(super) test_pump: super::test_support::TestPump,
}

impl SubtitlePresenter {
    /// Создаёт презентатор со стилем и настройками из конфига.
    pub(in crate::state::subtitles) fn new(
        system_language: Option<SubtitleLanguage>,
        style: &UserSubtitleStyle,
        settings: PresenterSettings,
        wake_port: AppWakePort,
    ) -> Self {
        Self {
            link: WorkerLink::new(wake_port),
            appearance: style.to_appearance(),
            ass: AssPresentation::from_style(style),
            settings,
            system_language,
            queue: PreparedQueue::new(queue_capacity(settings)),
            inputs_revision: RenderInputsRevision::INITIAL,
            last_ass_options: None,
            sent_look: None,
            mirror: MirrorSync::default(),
            clock: ClockTracker::default(),
            observation: PlaybackObservation::default(),
            sent_presentation: None,
            expected_target: None,
            invalidations: VecDeque::new(),
            last_frame: None,
            scratch_active_events: Vec::new(),
            shown: None,
            stats: PresentationStats::default(),
            lateness: LatenessMonitor::default(),
            #[cfg(test)]
            test_pump: super::test_support::TestPump::default(),
        }
    }

    /// Есть ли растеризатор (то есть нужна ли новая библиотека шрифтов следующей задаче).
    pub(in crate::state::subtitles) fn has_rasterizer(&self) -> bool {
        self.link.is_running()
    }

    /// Запускает поток-рисовальщик поверх библиотеки, подготовленной в фоне. Всё, что было
    /// нарисовано прежним потоком, забывается.
    pub(in crate::state::subtitles) fn install_font_library(&mut self, fonts: SubtitleFontLibrary) {
        self.queue = PreparedQueue::new(queue_capacity(self.settings));
        self.mirror.reset();
        self.sent_look = None;
        self.sent_presentation = None;
        self.expected_target = None;
        self.shown = None;
        self.inputs_revision = self.inputs_revision.next();
        self.link.start(
            fonts,
            WorkerSettings {
                raster_limits: self.settings.raster_limits,
                system_language: self.system_language.clone(),
                prerender_frames: PrerenderFrames::new(self.settings.prerender_frames),
                prepared_overlays: queue_capacity(self.settings).get(),
            },
        );
    }

    /// Дешёвая фаза установки вложенных шрифтов media. `Err` — потока ещё нет (вызывающий
    /// держит подготовленные шрифты, пока он не появится). Отчёт об установке приходит от
    /// потока событием и пишется в лог при приёме. Показанное забывается: шрифт реплик мог
    /// измениться.
    pub(in crate::state::subtitles) fn install_attached_fonts(
        &mut self,
        prepared: PreparedAttachedFonts,
        limits: &AttachedFontLimits,
    ) -> Result<(), PreparedAttachedFonts> {
        if !self.link.is_running() {
            return Err(prepared);
        }
        self.forget_shown_overlay();
        let look = self.unsent_look_for_command();
        let look_marker = look.as_ref().map(|look| (look.revision, look.ass));
        // Потеря потока здесь становится отказом внутри `send`; шрифты в этом случае не нужны.
        let sent = self.link.send(WorkerCommand::InstallAttachedFonts {
            prepared,
            limits: *limits,
            look,
        });
        if sent.is_ok() && look_marker.is_some() {
            self.sent_look = look_marker;
        }
        Ok(())
    }

    /// Снимает вложенные шрифты прежнего media (встроенные и системные не затрагиваются) и
    /// забывает показанный overlay. Итог поток присылает событием.
    pub(in crate::state::subtitles) fn clear_attached_fonts(&mut self) {
        self.forget_shown_overlay();
        if !self.link.is_running() {
            return;
        }
        let look = self.unsent_look_for_command();
        let look_marker = look.as_ref().map(|look| (look.revision, look.ass));
        let sent = self.link.send(WorkerCommand::ClearAttachedFonts { look });
        if sent.is_ok() && look_marker.is_some() {
            self.sent_look = look_marker;
        }
    }

    /// Входы рисования для команды, не связанной с кадром (шрифты): ревизия выросла, а
    /// следующий `Present` придёт только с кадром. Без кадров (настроек ASS ещё нет) - `None`.
    fn unsent_look_for_command(&mut self) -> Option<LookInputs> {
        let ass_options = self.last_ass_options?;
        self.unsent_look(ass_options)
    }

    /// Забывает показанный overlay и очередь (смена media или дорожки): прежняя картинка не
    /// может «просочиться». Ревизия входов растёт: поток обнуляет память о выданном.
    pub(in crate::state::subtitles) fn forget_shown_overlay(&mut self) {
        self.queue.clear();
        self.shown = None;
        self.inputs_revision = self.inputs_revision.next();
        // Без кадра с актуальной геометрией часы для seek/scrub слать не из чего.
        self.last_frame = None;
    }

    /// Забывает всё, что относилось к прежнему media: холст, настройки ASS, шаг кадров,
    /// отправленные часы и недавние изменения дорожек. Следующее media не получит часы с
    /// холстом или шагом предыдущего.
    fn forget_playback_context(&mut self) {
        self.last_frame = None;
        self.last_ass_options = None;
        self.clock = ClockTracker::default();
        self.sent_presentation = None;
        self.expected_target = None;
        self.invalidations.clear();
    }

    /// Просит поток забыть зеркала дорожек (media исчезло).
    pub(in crate::state::subtitles) fn release_mirrored_tracks(&mut self) {
        let link = &mut self.link;
        // Потеря потока при отправке уже стала отказом; зеркала всё равно забыты.
        let _released = self.mirror.release_all(&mut |command| link.send(command));
        self.mirror.reset();
        self.forget_playback_context();
    }

    /// Меняет внешний вид простых субтитров; одинаковый — no-op.
    ///
    /// Картинки старого вида остаются опорой фоллбэка, пока не придут новые: смена не мигает.
    pub(in crate::state::subtitles) fn replace_appearance(
        &mut self,
        appearance: SubtitleAppearance,
    ) {
        if self.appearance == appearance {
            return;
        }
        self.appearance = appearance;
        self.inputs_revision = self.inputs_revision.next();
    }

    /// Применяет стиль пользователя целиком: внешний вид текста и настройки ASS (масштаб
    /// текста, «мой стиль для ASS»). Неизменный стиль — no-op.
    pub(in crate::state::subtitles) fn replace_style(&mut self, style: &UserSubtitleStyle) {
        self.replace_appearance(style.to_appearance());
        let ass = AssPresentation::from_style(style);
        if self.ass != ass {
            self.ass = ass;
            self.inputs_revision = self.inputs_revision.next();
        }
    }

    /// Показывает ли готовый overlay реплики ASS (тогда слой размещается по правилам ASS).
    pub(in crate::state::subtitles) fn shown_overlay_contains_ass(&self) -> bool {
        self.shown_overlay().is_some() && self.queue.shown_contains_ass()
    }

    /// Запоминает настройки для СЛЕДУЮЩЕГО потока. Действующий не трогает.
    pub(in crate::state::subtitles) fn replace_settings(&mut self, settings: PresenterSettings) {
        self.settings = settings;
    }

    /// Останавливает поток (вместе с библиотекой шрифтов и кэшами). Следующая загрузка
    /// построит новый с актуальными настройками. Вызывает владелец только на смене media.
    pub(in crate::state::subtitles) fn discard_rasterizer(&mut self) {
        self.link.stop();
        self.mirror.reset();
        self.sent_look = None;
        self.forget_playback_context();
        self.forget_shown_overlay();
    }

    /// Запоминает наблюдение за плеером (режим, скорость, оценка шага кадра).
    pub(in crate::state::subtitles) fn observe_playback(
        &mut self,
        observation: PlaybackObservation,
    ) {
        // Новый проход чтения демуксера - разрыв потока кадров (seek, scrub, потеря пакета).
        if observation.read_run != self.observation.read_run {
            self.clock.note_discontinuity();
        }
        self.observation = observation;
    }

    /// Учитывает кадр при выводе шага кадров. Во время seek/scrub соседние кадры - это
    /// перескоки по таймлайну, а не ход видео: шаг из них не выводится, и следующий кадр
    /// после scrub не сравнивается с последним кадром превью.
    fn note_frame_for_step(&mut self, frame_pts: Duration) {
        if self.observation.mode.has_target() {
            self.clock.note_discontinuity();
        } else {
            self.clock.note_frame(frame_pts);
        }
    }

    /// Режим воспроизведения опирается на цель (seek/scrub), а не на показанный кадр.
    pub(in crate::state::subtitles) fn playback_follows_target(&self) -> bool {
        self.observation.mode.has_target()
    }

    /// Отказ потока, о котором владелец ещё не знал (отдаётся один раз).
    pub(in crate::state::subtitles) fn take_failure(&mut self) -> Option<RenderWorkerFailure> {
        self.link.take_failure()
    }

    /// Опрос потока по wake-событию: принимает готовое. `true` — показ текущего кадра мог
    /// измениться (нужна перерисовка: на паузе новый кадр сам не придёт).
    pub(in crate::state::subtitles) fn poll_worker(&mut self) -> bool {
        self.link.prune_retired();
        self.ingest_events()
    }

    /// Показанный overlay, если он не пуст.
    pub(in crate::state::subtitles) fn shown_overlay(&self) -> Option<&SubtitleOverlay> {
        self.shown
            .as_ref()
            .map(|shown| &shown.overlay)
            .filter(|overlay| !overlay.is_empty())
    }

    /// Главный вход: overlay для кадра или `None`, если рисовать нечего.
    pub(in crate::state::subtitles) fn overlay_for_frame(
        &mut self,
        track: &ActiveSubtitleTrack,
        frame: &SubmittedFrameForSubtitles,
    ) -> Option<&SubtitleOverlay> {
        self.prepare_overlay(TrackRole::Display, track, frame);
        self.shown_overlay()
    }

    /// Overlay кадра в режиме правки: настоящая реплика, а если на кадре её нет - образец.
    ///
    /// Приоритет у настоящей реплики (решение владельца 2): образец подставляется только когда
    /// `primary` отсутствует либо на этом кадре у неё нет активных реплик. Часы уходят потоку
    /// только для выбранной дорожки (иначе они дёргались бы между двумя на каждом кадре).
    pub(in crate::state::subtitles) fn overlay_for_frame_or_sample(
        &mut self,
        primary: Option<&ActiveSubtitleTrack>,
        sample: &ActiveSubtitleTrack,
        frame: &SubmittedFrameForSubtitles,
    ) -> Option<&SubtitleOverlay> {
        let primary_has_cues = primary.is_some_and(|track| {
            let lookup_time = track.lookup_time_for(frame.frame_pts);
            self.collect_active_events(track, lookup_time)
        });
        match primary {
            Some(track) if primary_has_cues => {
                self.prepare_overlay(TrackRole::Display, track, frame);
            }
            _ => {
                self.prepare_overlay(TrackRole::Sample, sample, frame);
            }
        }
        self.shown_overlay()
    }

    /// Seek/scrub без нового кадра: сообщает потоку цель по последнему известному кадру,
    /// чтобы картинка цели была готова к первому кадру после seek.
    pub(in crate::state::subtitles) fn refresh_target_clock(
        &mut self,
        track: &ActiveSubtitleTrack,
    ) {
        let Some(last_frame) = self.last_frame else {
            return;
        };
        if !self.link.is_running() || !self.observation.mode.has_target() {
            return;
        }
        let Some(ass_options) = self.last_ass_options else {
            return;
        };
        self.sync_track(TrackRole::Display, track);
        self.send_presentation(
            track,
            last_frame.canvas,
            last_frame.lookup_time,
            ass_options,
        );
    }

    /// Собирает активные на `lookup_time` реплики в буфер; `true`, если они есть.
    fn collect_active_events(
        &mut self,
        track: &ActiveSubtitleTrack,
        lookup_time: SubtitleTime,
    ) -> bool {
        track
            .timeline()
            .collect_active_at(lookup_time, &mut self.scratch_active_events);
        !self.scratch_active_events.is_empty()
    }

    /// Готовит `shown` для кадра по дорожке и сообщает, были ли реплики на кадре.
    fn prepare_overlay(
        &mut self,
        role: TrackRole,
        track: &ActiveSubtitleTrack,
        frame: &SubmittedFrameForSubtitles,
    ) -> ActiveCues {
        let Some(canvas) = canvas_for_picture(frame.picture_rect()) else {
            self.shown = None;
            return ActiveCues::None;
        };
        let lookup_time = track.lookup_time_for(frame.frame_pts);
        let has_cues = self.collect_active_events(track, lookup_time);
        self.note_frame_for_step(frame.frame_pts);
        self.last_frame = Some(LastFrame {
            canvas,
            lookup_time,
        });
        let cues = if has_cues {
            ActiveCues::Present
        } else {
            ActiveCues::None
        };

        if !self.link.is_running() {
            // Потока нет (шрифты ещё не доехали или отказ): показать нечего.
            self.shown = None;
            return cues;
        }
        // Свежие результаты видны уже на этом кадре.
        self.ingest_events();
        let ass_options = self.ass.render_options(frame.ass_storage_size());
        self.sync_track(role, track);
        self.send_presentation(track, canvas, lookup_time, ass_options);
        #[cfg(test)]
        self.settle_worker_for_test();

        if !has_cues {
            // На кадре нет реплик: показанного больше нет. Поток всё равно готовит наперёд.
            self.shown = None;
            return cues;
        }
        self.select_picture(track, canvas, lookup_time);
        cues
    }

    /// Выбирает картинку из очереди и запоминает её как показанную.
    fn select_picture(
        &mut self,
        track: &ActiveSubtitleTrack,
        canvas: CanvasSize,
        lookup_time: SubtitleTime,
    ) {
        let identity = OverlayIdentity {
            source: track.source_id(),
            canvas,
            inputs: self.inputs_revision,
        };
        let was_late = match self
            .queue
            .select(identity, lookup_time, &self.scratch_active_events)
        {
            Selection::Exact(overlay) => {
                let same = self
                    .shown
                    .as_ref()
                    .is_some_and(|shown| shown.is_same_picture_as(overlay));
                if same {
                    self.stats.reused_frames += 1;
                } else {
                    // Копия только при смене картинки; на обычном кадре аллокаций нет.
                    self.shown = Some(ShownOverlay {
                        overlay: overlay.clone(),
                    });
                }
                false
            }
            Selection::StaleWithoutEnded(overlay) => {
                self.shown = Some(ShownOverlay { overlay });
                true
            }
            Selection::Nothing => {
                self.shown = None;
                true
            }
        };
        if was_late {
            self.stats.late_pictures += 1;
            tracing::debug!(
                canvas_width = canvas.width(),
                canvas_height = canvas.height(),
                "The subtitle picture was not ready for its frame; showing the previous one"
            );
        }
        if self.lateness.record_frame(was_late) {
            tracing::warn!(
                late_total = self.lateness.total_late(),
                "Subtitle pictures are systematically late for their frames"
            );
        }
    }

    /// Входы рисования, которые ещё не отправлены потоку (`None` — поток уже знает актуальные).
    /// Смена настроек ASS (размер картинки видео) тоже меняет ревизию: прежние картинки не
    /// годятся для точного выбора.
    fn unsent_look(&mut self, ass_options: AssRenderOptions) -> Option<LookInputs> {
        if self.last_ass_options != Some(ass_options) {
            if self.last_ass_options.is_some() {
                self.inputs_revision = self.inputs_revision.next();
            }
            self.last_ass_options = Some(ass_options);
        }
        if self.sent_look == Some((self.inputs_revision, ass_options)) {
            return None;
        }
        Some(LookInputs {
            revision: self.inputs_revision,
            appearance: self.appearance,
            ass: ass_options,
        })
    }

    /// Держит зеркало дорожки у потока в актуальном состоянии (см. `mirror_sync`) и
    /// выбрасывает из очереди картинки, чей срок задели дописанные реплики.
    fn sync_track(&mut self, role: TrackRole, track: &ActiveSubtitleTrack) {
        let link = &mut self.link;
        let outcome = self
            .mirror
            .sync(role, track, &mut |command| link.send(command));
        let source = track.source_id();
        match outcome {
            Ok(SyncOutcome::Updated {
                serial,
                appended,
                shortened,
            }) => {
                if let Some(span) = appended {
                    self.note_track_invalidation(TrackInvalidation {
                        serial,
                        source,
                        span,
                        kind: InvalidationKind::Overwritten,
                    });
                }
                if let Some(span) = shortened {
                    self.note_track_invalidation(TrackInvalidation {
                        serial,
                        source,
                        span,
                        kind: InvalidationKind::EventsShortened,
                    });
                }
            }
            // Дорожка отправлена целиком: всё нарисованное для неё раньше недействительно.
            Ok(SyncOutcome::Replaced { serial }) => {
                self.note_track_invalidation(TrackInvalidation {
                    serial,
                    source,
                    span: TimeSpan::everything(),
                    kind: InvalidationKind::Overwritten,
                });
            }
            Ok(SyncOutcome::Unchanged) | Err(_) => {}
        }
    }

    /// Отправляет «часы показа», если они изменились с прошлой отправки.
    fn send_presentation(
        &mut self,
        track: &ActiveSubtitleTrack,
        canvas: CanvasSize,
        lookup_time: SubtitleTime,
        ass_options: AssRenderOptions,
    ) {
        let look = self.unsent_look(ass_options);
        let target = RenderTarget {
            source: track.source_id(),
            canvas,
        };
        let clock = self.clock.clock_for(lookup_time, &self.observation, track);
        // Опорное время планирования - единое для потока и очереди: «время ушло» считается
        // от него (цель в seek/scrub), а не от показанного кадра.
        self.queue.note_planning_anchor(clock.planning_anchor());
        // Ожидаемая цель обновляется на каждом кадре, даже если часы не менялись.
        self.expected_target = Some(target);
        let presentation = SentPresentation { target, clock };
        if look.is_none() && self.sent_presentation == Some(presentation) {
            return;
        }
        let revision_and_ass = look.as_ref().map(|look| (look.revision, look.ass));
        if self
            .link
            .send(WorkerCommand::Present {
                target,
                clock,
                look,
            })
            .is_ok()
        {
            self.sent_presentation = Some(presentation);
            if revision_and_ass.is_some() {
                self.sent_look = revision_and_ass;
            }
        }
    }

    /// Только тесты: осталось ли в презентаторе что-то от прежних кадров.
    #[cfg(test)]
    pub(super) fn remembers_playback_context_impl(&self) -> bool {
        self.last_frame.is_some()
            || self.last_ass_options.is_some()
            || self.clock.has_observed_step()
            || self.sent_presentation.is_some()
    }
}

/// Размер очереди готовых картинок и памяти потока о выданном (один и тот же у обеих сторон).
///
/// Не меньше `prerender_frames + 1`: текущий кадр и весь горизонт должны помещаться
/// одновременно, иначе планировщик вытеснял бы собственные результаты. Поэтому горизонт
/// никогда не зажимается молча: вместо этого растёт очередь.
fn queue_capacity(settings: PresenterSettings) -> NonZeroUsize {
    let configured = usize::try_from(settings.prepared_overlays).unwrap_or(usize::MAX);
    let for_horizon = usize::try_from(settings.prerender_frames)
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    NonZeroUsize::new(configured.max(for_horizon)).unwrap_or(NonZeroUsize::MIN)
}

/// Холст = размер картинки видео, округлённый до целых пикселей (контракт GPU-прохода:
/// 1 пиксель холста = 1 пиксель экрана). `None` для вырожденной картинки.
fn canvas_for_picture(picture_rect: PictureRect) -> Option<CanvasSize> {
    let width = picture_rect.width.round();
    let height = picture_rect.height.round();
    // Сравнение в форме `>=` сознательно: NaN не проходит его и даёт `None`.
    if !(width >= 1.0 && height >= 1.0) {
        return None;
    }
    // `as` насыщает значения вне диапазона; сверхбольшой холст отсечёт растеризатор лимитом.
    CanvasSize::new(width as u32, height as u32)
}
