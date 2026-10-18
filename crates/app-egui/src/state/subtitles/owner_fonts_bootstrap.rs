//! Запуск растеризатора «по требованию»: для образца в режиме правки и для встроенных дорожек.
//!
//! Растеризатор (с библиотекой шрифтов ~90 мс) обычно строит первая загруженная дорожка. Если
//! у видео дорожек нет (или субтитры выключены), он не появится никогда, а образцу он нужен.
//! Поэтому сессия правки сама заказывает библиотеку отдельной фоновой задачей
//! (`SidecarJobTask::BuildFontsOnly`). Владелец задачи - `SubtitleOwner`: она не привязана
//! к media и переживает его смену. Образец появляется чуть позже входа в режим (когда задача
//! отработает); до этого слой просто пуст.

use super::SubtitleOwner;
use super::job_runner::{SidecarJob, SidecarJobPoll};
use super::sidecar_job::{FontLibraryPlan, SidecarJobOutcome, SidecarJobRequest, SidecarJobTask};

impl SubtitleOwner {
    /// Заказывает растеризатор, если его нет и заказ ещё не идёт. Нужен образцу правки и
    /// media со встроенными дорожками (их реплики показывает тот же растеризатор, а файл
    /// рядом, который обычно строит библиотеку, может отсутствовать).
    pub(super) fn request_rasterizer_in_background(&mut self) {
        if self.presenter.has_rasterizer() || self.fonts_bootstrap_job.is_some() {
            return;
        }
        let request = SidecarJobRequest {
            task: SidecarJobTask::BuildFontsOnly,
            fonts: self.font_plan_for_new_library,
            limits: self.settings.load_limits,
        };
        debug_assert!(
            request.fonts != FontLibraryPlan::ReuseExisting,
            "растеризатора нет, значит библиотеку нужно строить"
        );
        match SidecarJob::spawn(request, self.wake_port.clone()) {
            Ok(job) => self.fonts_bootstrap_job = Some(job),
            Err(failure) => {
                // Образец не появится, но правка остаётся рабочей: ползунки и позиция живы.
                tracing::warn!(%failure, "Subtitle fonts could not be prepared for the sample");
            }
        }
    }

    /// Забирает результат заказа шрифтов. `true` - видимое состояние изменилось.
    pub(super) fn poll_fonts_bootstrap(&mut self) -> bool {
        let Some(job) = &self.fonts_bootstrap_job else {
            return false;
        };
        let SidecarJobPoll::Finished(outcome) = job.poll() else {
            return false;
        };
        self.fonts_bootstrap_job = None;
        match outcome {
            SidecarJobOutcome::FontsBuilt(fonts) => {
                self.install_fonts_if_needed(fonts);
                true
            }
            SidecarJobOutcome::Cancelled => false,
            SidecarJobOutcome::Failed(failure) => {
                tracing::warn!(%failure, "Subtitle fonts could not be prepared for the sample");
                false
            }
            SidecarJobOutcome::Discovered(_)
            | SidecarJobOutcome::FileLoaded { .. }
            | SidecarJobOutcome::WebTrackLoaded { .. } => {
                tracing::error!("A fonts task returned a foreign result; ignoring it");
                false
            }
        }
    }

    /// Идёт ли заказ шрифтов (владельцу нужны wake-события).
    pub(super) fn has_running_fonts_bootstrap(&self) -> bool {
        self.fonts_bootstrap_job.is_some()
    }

    /// Идёт ли любая фоновая работа со шрифтами: библиотека или вложения media.
    pub(super) fn has_running_font_jobs(&self) -> bool {
        self.has_running_fonts_bootstrap() || self.attached_fonts.has_running_work()
    }
}
