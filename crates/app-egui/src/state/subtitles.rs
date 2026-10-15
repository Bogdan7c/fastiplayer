//! Владелец субтитров текущего media в `AppState`.
//!
//! # Архитектура
//! - `SubtitleOwner` (этот файл) владеет жизненным циклом: какое media сейчас, идёт ли
//!   загрузка, загружена ли дорожка. Он единственный решает, что показывать, и скрывает
//!   субтитры при любом сомнении о принадлежности кадра текущему media.
//! - `sidecar_job` + `job_runner`: отдельная фоновая задача «найти файл рядом с видео,
//!   прочитать, разобрать», запускается ПОСЛЕ установки локального файла в плеер. Она
//!   привязана к `MediaInstanceId` и не входит в путь подготовки media.
//! - `active_track`: загруженный таймлайн + идентичность источника + сдвиг синхронизации.
//! - `presentation`: решение по кадру (реплики по pts кадра -> overlay растеризатора).
//! - `limits`: все лимиты и бюджеты (переедут в конфиг в сессии 04).
//!
//! # Точка запуска (единая для всех способов открыть файл)
//! Любой путь установки media (кнопка Open, drag&drop, playlist next, пересборка после
//! смены настроек, resume) заканчивается в `AppState::record_installed_media*`
//! (`state/media_jobs.rs`), где одновременно известны `MediaInstanceId` и источник.
//! Оттуда вызывается `start_subtitles_for_installed_media`. Сброс — в
//! `clear_released_installed_media_source` и `record_cleared_media_after_exact_reset`.
//!
//! # Правило «субтитры старого media не показываются ни кадра»
//! Overlay отдаётся только если одновременно:
//! 1. дорожка загружена для того же `MediaInstanceId`, что в текущем snapshot плеера;
//! 2. `render_generation` отправляемого кадра совпадает с `snapshot.render_generation`.
//!
//! Пункт 2 надёжен без мерцания в обычном воспроизведении: `acquire_present_frame_for_render`
//! уже отбрасывает lease с другим поколением, а кэшированные кадры проверяются против того же
//! snapshot, поэтому в штатной работе равенство выполняется всегда. А поколение растёт при
//! установке нового media (staged commit) и сбросе media, поэтому кадр из прошлого media
//! (например, кэш) не пройдёт проверку, даже если snapshot уже сменился. Snapshot читается
//! один раз в начале кадра, а lease берётся позже в том же кадре; рассинхрон возможен только
//! на границе смены media/backend, где скрытие и нужно.
//!
//! Кадр без identity (stale-lease во время seek) субтитры не получает вовсе: старая картинка
//! остаётся на экране без реплики, первый свежий кадр показывает реплику по своему pts.

mod active_track;
mod job_runner;
mod limits;
mod presentation;
mod sidecar_job;
mod system_language;
#[cfg(test)]
mod tests;

use std::path::PathBuf;

use player_core::MediaInstanceId;
use subtitle_core::{SubtitleLanguage, SubtitleSyncOffset};
use subtitle_raster::{CueSourceId, SubtitleOverlay};

use self::active_track::ActiveSubtitleTrack;
use self::job_runner::{SidecarJob, SidecarJobPoll};
#[cfg(test)]
use self::presentation::PresentationStats;
pub(crate) use self::presentation::SubmittedFrameForSubtitles;
use self::presentation::SubtitlePresenter;
use self::sidecar_job::{
    FontLibraryPlan, SidecarJobOutcome, SidecarJobRequest, SubtitleLoadFailure,
};
pub(super) use self::system_language::read_system_language;
use super::{ActiveMediaSource, AppState, InstalledSingleMediaOpen};
use crate::app_wake::AppWakePort;
use player_core::MediaInstallCompletion;

/// Откуда у только что установленного media могут быть субтитры.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SubtitleMediaSource {
    /// Локальный файл: рядом с ним ищем файлы субтитров.
    LocalFile(PathBuf),
    /// Не локальный источник (web и прочее): файлов рядом нет, субтитры очищены.
    NotLocal,
}

/// Какое media сейчас видит плеер: сверяется с владельцем перед показом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CurrentMediaForSubtitles {
    /// `media_instance_id` из текущего snapshot (`None` — media нет).
    pub(super) media_instance_id: Option<MediaInstanceId>,
    /// `render_generation` из текущего snapshot.
    pub(super) render_generation: u64,
}

/// Что сделали с готовым результатом фоновой задачи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JobResultDisposition {
    /// Результат принят (видимое состояние могло измениться).
    Applied,
    /// Результат относится к другому media (или задача отменена) и выброшен.
    DroppedStale,
}

/// Жизненный цикл субтитров текущего media.
enum SubtitleLifecycle {
    /// Media нет или оно не локальное.
    Idle,
    /// Фоновая задача ещё работает.
    Loading {
        media_instance_id: MediaInstanceId,
        job: SidecarJob,
    },
    /// Дорожка загружена и может показываться.
    Ready {
        media_instance_id: MediaInstanceId,
        track: ActiveSubtitleTrack,
    },
    /// Для этого media нет файла, который можно показать сейчас (не ошибка).
    NoSubtitleFile { media_instance_id: MediaInstanceId },
    /// Дорожка найдена, но недоступна по типизированной причине.
    Unavailable {
        media_instance_id: MediaInstanceId,
        #[cfg_attr(
            not(test),
            expect(dead_code, reason = "причину покажет меню субтитров (сессия 04)")
        )]
        failure: SubtitleLoadFailure,
    },
}

impl SubtitleLifecycle {
    /// Media, которому принадлежит состояние.
    fn media_instance_id(&self) -> Option<MediaInstanceId> {
        match self {
            Self::Idle => None,
            Self::Loading {
                media_instance_id, ..
            }
            | Self::Ready {
                media_instance_id, ..
            }
            | Self::NoSubtitleFile { media_instance_id }
            | Self::Unavailable {
                media_instance_id, ..
            } => Some(*media_instance_id),
        }
    }
}

/// Краткий вид состояния для тестов и диагностики (без внутренних типов).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SubtitleLifecycleView {
    Idle,
    Loading,
    Ready,
    NoSubtitleFile,
    Unavailable,
}

/// Владелец субтитров (см. документацию модуля).
pub(super) struct SubtitleOwner {
    wake_port: AppWakePort,
    lifecycle: SubtitleLifecycle,
    presenter: SubtitlePresenter,
    /// Задачи, которые больше не нужны, но ещё работают: ждём их завершения без блокировки UI.
    retiring_jobs: Vec<SidecarJob>,
    /// Следующий идентификатор источника реплик для кэша растеризатора.
    next_source_id: u64,
    /// Какие шрифты строить новой задаче; тесты выбирают встроенные.
    font_plan_for_new_library: FontLibraryPlan,
}

impl SubtitleOwner {
    /// Создаёт владельца без media. `system_language` — язык системы, прочитанный один раз
    /// на границе сборки приложения ([`read_system_language`]); он нужен как запасной язык
    /// текста, когда дорожка своего не объявляет.
    pub(super) fn new(wake_port: AppWakePort, system_language: Option<SubtitleLanguage>) -> Self {
        Self {
            wake_port,
            lifecycle: SubtitleLifecycle::Idle,
            presenter: SubtitlePresenter::new(system_language),
            retiring_jobs: Vec::new(),
            next_source_id: 0,
            font_plan_for_new_library: FontLibraryPlan::BuildWithSystemFonts,
        }
    }

    /// Новое media установлено: прежние субтитры сброшены, для локального файла стартует
    /// фоновая загрузка. Вызывается из единой точки установки media.
    pub(super) fn begin_media(
        &mut self,
        media_instance_id: MediaInstanceId,
        source: SubtitleMediaSource,
    ) {
        self.reset_to_idle();
        let SubtitleMediaSource::LocalFile(video_path) = source else {
            // Не локальный источник: файлов рядом нет, это штатное «без субтитров».
            self.lifecycle = SubtitleLifecycle::NoSubtitleFile { media_instance_id };
            return;
        };
        let fonts = if self.presenter.has_rasterizer() {
            FontLibraryPlan::ReuseExisting
        } else {
            self.font_plan_for_new_library
        };
        let request = SidecarJobRequest { video_path, fonts };
        self.lifecycle = match SidecarJob::spawn(request, self.wake_port.clone()) {
            Ok(job) => SubtitleLifecycle::Loading {
                media_instance_id,
                job,
            },
            Err(failure) => {
                tracing::warn!(%failure, "Subtitles are unavailable; playback continues");
                SubtitleLifecycle::Unavailable {
                    media_instance_id,
                    failure,
                }
            }
        };
    }

    /// Media исчезло (release/reset): субтитры и задача сброшены.
    pub(super) fn clear_media(&mut self) {
        self.reset_to_idle();
    }

    /// Останавливает текущую задачу (уходит в `retiring_jobs`) и забывает дорожку и overlay.
    fn reset_to_idle(&mut self) {
        let previous = std::mem::replace(&mut self.lifecycle, SubtitleLifecycle::Idle);
        if let SubtitleLifecycle::Loading { job, .. } = previous {
            job.cancel();
            self.retiring_jobs.push(job);
        }
        self.presenter.forget_shown_overlay();
    }

    /// Неблокирующий опрос задач. `true` — видимое состояние изменилось (нужна перерисовка).
    ///
    /// Всегда опустошает ящики (в том числе у уходящих задач), иначе wake-порт остался бы
    /// «взведённым» и следующие результаты не будили бы окно.
    pub(super) fn poll_jobs(&mut self) -> bool {
        // Результаты уходящих задач выбрасываем; завершённые потоки освобождаем.
        self.retiring_jobs.retain(|job| {
            let _discarded = job.poll();
            !job.is_finished()
        });

        let finished = match &self.lifecycle {
            SubtitleLifecycle::Loading {
                media_instance_id,
                job,
            } => match job.poll() {
                SidecarJobPoll::Pending => None,
                SidecarJobPoll::Finished(outcome) => Some((*media_instance_id, outcome)),
            },
            _ => {
                // Задач, которых ждёт владелец, нет: сбрасываем «взведённость» порта.
                self.wake_port.acknowledge_abandoned_mailbox();
                None
            }
        };
        match finished {
            Some((media_instance_id, outcome)) => {
                self.accept_job_outcome(media_instance_id, outcome) == JobResultDisposition::Applied
            }
            None => false,
        }
    }

    /// Применяет результат задачи, запущенной для `media_instance_id`.
    ///
    /// Поздний результат для другого media выбрасывается: проверка идёт по текущему
    /// состоянию, а не по порядку доставки.
    fn accept_job_outcome(
        &mut self,
        media_instance_id: MediaInstanceId,
        outcome: SidecarJobOutcome,
    ) -> JobResultDisposition {
        if self.lifecycle.media_instance_id() != Some(media_instance_id)
            || !matches!(self.lifecycle, SubtitleLifecycle::Loading { .. })
        {
            tracing::debug!("Dropped a subtitle result for media that is no longer current");
            return JobResultDisposition::DroppedStale;
        }
        // Завершённая задача больше не нужна: освобождаем поток.
        let previous = std::mem::replace(&mut self.lifecycle, SubtitleLifecycle::Idle);
        drop(previous);

        self.lifecycle = match outcome {
            SidecarJobOutcome::Cancelled => return JobResultDisposition::DroppedStale,
            SidecarJobOutcome::NoRenderableCandidate {
                skipped_unsupported_format,
            } => {
                tracing::debug!(skipped_unsupported_format, "No subtitle file to show");
                SubtitleLifecycle::NoSubtitleFile { media_instance_id }
            }
            SidecarJobOutcome::Failed(failure) => {
                tracing::warn!(%failure, "Subtitle file is unavailable; playback continues");
                SubtitleLifecycle::Unavailable {
                    media_instance_id,
                    failure,
                }
            }
            SidecarJobOutcome::Loaded(loaded) => {
                let loaded = *loaded;
                if let Some(fonts) = loaded.fonts {
                    self.presenter.install_font_library(fonts);
                }
                tracing::debug!(
                    format = ?loaded.format,
                    file_name = ?loaded.file_name,
                    events = loaded.timeline.len(),
                    "Subtitle track loaded"
                );
                let source_id = self.allocate_source_id();
                SubtitleLifecycle::Ready {
                    media_instance_id,
                    track: ActiveSubtitleTrack::new(
                        source_id,
                        loaded.timeline,
                        SubtitleSyncOffset::ZERO,
                        loaded.language,
                    ),
                }
            }
        };
        self.presenter.forget_shown_overlay();
        JobResultDisposition::Applied
    }

    /// Выдаёт новый идентификатор источника реплик.
    fn allocate_source_id(&mut self) -> CueSourceId {
        let source_id = CueSourceId::new(self.next_source_id);
        self.next_source_id = self.next_source_id.saturating_add(1);
        source_id
    }

    /// Overlay для кадра, который прямо сейчас уходит в renderer, или `None`.
    ///
    /// Реализует правило из документации модуля: другое media или другое поколение
    /// ресурсов рендера — субтитров нет.
    pub(super) fn overlay_for_frame(
        &mut self,
        current: CurrentMediaForSubtitles,
        frame: &SubmittedFrameForSubtitles,
    ) -> Option<&SubtitleOverlay> {
        let SubtitleLifecycle::Ready {
            media_instance_id,
            track,
        } = &self.lifecycle
        else {
            return None;
        };
        if current.media_instance_id != Some(*media_instance_id)
            || frame.render_generation() != current.render_generation
        {
            return None;
        }
        self.presenter.overlay_for_frame(track, frame)
    }

    /// Счётчики презентации.
    #[cfg(test)]
    fn presentation_stats(&self) -> PresentationStats {
        self.presenter.stats()
    }

    /// Текущее состояние для тестов.
    #[cfg(test)]
    fn lifecycle_view(&self) -> SubtitleLifecycleView {
        match self.lifecycle {
            SubtitleLifecycle::Idle => SubtitleLifecycleView::Idle,
            SubtitleLifecycle::Loading { .. } => SubtitleLifecycleView::Loading,
            SubtitleLifecycle::Ready { .. } => SubtitleLifecycleView::Ready,
            SubtitleLifecycle::NoSubtitleFile { .. } => SubtitleLifecycleView::NoSubtitleFile,
            SubtitleLifecycle::Unavailable { .. } => SubtitleLifecycleView::Unavailable,
        }
    }

    /// Причина недоступности для тестов.
    #[cfg(test)]
    fn unavailable_failure(&self) -> Option<&SubtitleLoadFailure> {
        match &self.lifecycle {
            SubtitleLifecycle::Unavailable { failure, .. } => Some(failure),
            _ => None,
        }
    }

    /// Счётчики растеризатора для тестов.
    #[cfg(test)]
    fn rasterizer_statistics(&self) -> Option<subtitle_raster::RasterStatistics> {
        self.presenter.rasterizer_statistics()
    }

    /// Для тестов: строить только встроенные шрифты (быстро, не зависит от машины).
    #[cfg(test)]
    fn use_embedded_fonts_only(&mut self) {
        self.font_plan_for_new_library = FontLibraryPlan::BuildEmbeddedOnly;
    }

    /// Для тестов: замена стиля.
    #[cfg(test)]
    fn replace_appearance(&mut self, appearance: subtitle_raster::SubtitleAppearance) {
        self.presenter.replace_appearance(appearance);
    }

    /// Для тестов: подмена загруженной дорожки без фоновой задачи не нужна — тесты идут
    /// через `accept_job_outcome`, как production.
    #[cfg(test)]
    fn retiring_job_count(&self) -> usize {
        self.retiring_jobs.len()
    }
}

/// Какой источник subtitles-владельцу соответствует источнику media.
fn subtitle_source_of(source: &ActiveMediaSource) -> SubtitleMediaSource {
    match source.physical_source() {
        ActiveMediaSource::LocalFile(path) => SubtitleMediaSource::LocalFile(path.clone()),
        ActiveMediaSource::Web(_) | ActiveMediaSource::PlaybackWindow { .. } => {
            SubtitleMediaSource::NotLocal
        }
    }
}

/// `MediaInstanceId` из receipt-а установки (`None`, если receipt не `Installed`).
pub(super) fn installed_media_instance_id(
    installed: &InstalledSingleMediaOpen,
) -> Option<MediaInstanceId> {
    match installed.completion {
        MediaInstallCompletion::Installed {
            media_instance_id, ..
        } => Some(media_instance_id),
        MediaInstallCompletion::Failed { .. } | MediaInstallCompletion::Cancelled { .. } => None,
    }
}

impl AppState {
    /// Media установлено в плеер: запускает (или сбрасывает) загрузку субтитров.
    ///
    /// Единая точка для всех способов открыть файл (см. документацию модуля). Без
    /// `MediaInstanceId` (receipt не `Installed`) субтитры очищаются.
    pub(super) fn start_subtitles_for_installed_media(
        &mut self,
        media_instance_id: Option<MediaInstanceId>,
        source: &ActiveMediaSource,
    ) {
        match media_instance_id {
            Some(media_instance_id) => self
                .subtitles
                .begin_media(media_instance_id, subtitle_source_of(source)),
            None => self.subtitles.clear_media(),
        }
    }

    /// Media исчезло из плеера: субтитры сброшены.
    pub(super) fn clear_subtitles_for_released_media(&mut self) {
        self.subtitles.clear_media();
    }

    /// Опрос фоновых задач субтитров (по wake-событию). `true` — нужна перерисовка.
    pub(crate) fn poll_subtitle_jobs(&mut self) -> bool {
        self.subtitles.poll_jobs()
    }

    /// Overlay субтитров для кадра, который прямо сейчас уходит в renderer.
    ///
    /// Читает `media_instance_id` и `render_generation` из последнего snapshot плеера,
    /// не раскрывая их внешнему коду.
    pub(crate) fn subtitle_overlay_for_submitted_frame(
        &mut self,
        frame: &SubmittedFrameForSubtitles,
    ) -> Option<&SubtitleOverlay> {
        let current = CurrentMediaForSubtitles {
            media_instance_id: self.last_player_snapshot.media_instance_id,
            render_generation: self.last_player_snapshot.render_generation,
        };
        self.subtitles.overlay_for_frame(current, frame)
    }
}
