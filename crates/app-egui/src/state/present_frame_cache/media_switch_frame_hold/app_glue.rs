//! Связка владельца кадра на смене медиа с `AppState`: кто начинает смену, куда уходит кадр,
//! выброшенный кэшем, что рисуется вместо пустого поля и где учитывается показанный кадр.
//!
//! Точки входа (каждая — намерение, а не доступ к полю):
//! - `begin_media_switch_frame_watch` — окно браузерного режима начинает ставить новое медиа
//!   (`browser_session::window::player_port`);
//! - `hand_discarded_frame_to_media_switch` — единственный сброс кэша кадра
//!   (`clear_cached_present_frame`) отдаёт сюда выброшенный кадр и причину;
//! - `media_switch_frame_for_empty_video` — обычный путь выбора кадра остался без видео
//!   (`frame_prepare::present::prepared_frame_without_current_video`);
//! - `media_switch_frame_content` + `note_media_switch_frame_presented` — учёт кадра, который
//!   правда ушёл на экран (`frame_prepare::present`);
//! - `media_switch_hold_wake_deadline` + `expire_due_media_switch_hold` — будильник и конец
//!   удержания по сроку на каждом пробуждении окна (`frame_prepare`, `frame_decision`);
//! - `release_media_switch_frame_hold` — живая смена бэкенда (замена рендера приходит сюда
//!   через сброс кэша `RuntimeDrop`).

use std::time::Instant;

use player_core::PlayerSnapshot;
use tracing::debug;

use super::super::{
    AppState, CachedPresentFrameDiscardReason, CachedRenderablePresentFrame, RenderablePresentFrame,
};
use super::report::log_media_switch_report;
use super::{
    HoldExpiry, HoldReleaseCause, MediaSwitchAbandonCause, MediaSwitchFrameContent, OpenBoundary,
    SwitchFrame,
};

impl SwitchFrame for RenderablePresentFrame {
    fn render_generation(&self) -> u64 {
        self.present_frame().render_generation()
    }

    fn is_same_frame(&self, other: &Self) -> bool {
        AppState::present_frame_identity(self.present_frame())
            == AppState::present_frame_identity(other.present_frame())
    }
}

/// Что значит причина сброса кэша для идущей смены медиа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum DiscardRole {
    /// «Мягкая» причина: внутри смены кадр можно держать на экране.
    MayHold,
    /// Кадр держать небезопасно, но смена (и замер) идут дальше.
    ReleasesHold(HoldReleaseCause),
    /// Кадра нового медиа не будет: смена кончается без него.
    EndsSwitch(MediaSwitchAbandonCause),
}

/// Роль причины сброса кэша. Деление — по смыслу причины: смена медиа сама по себе
/// (граница открытия, плеер ещё без кадра, новое поколение рендера) — мягкая; поломка
/// поверхности или рендера — отпускает кадр; конец медиа без замены — кончает смену.
pub(in crate::state) const fn discard_role(reason: CachedPresentFrameDiscardReason) -> DiscardRole {
    match reason {
        CachedPresentFrameDiscardReason::MediaOpenBoundary
        | CachedPresentFrameDiscardReason::CurrentVideoFrameMissing
        | CachedPresentFrameDiscardReason::SourceLabelChanged
        | CachedPresentFrameDiscardReason::RenderGenerationChanged
        | CachedPresentFrameDiscardReason::PlayerMediaOpenRequested
        | CachedPresentFrameDiscardReason::PlayerMediaOpened => DiscardRole::MayHold,
        CachedPresentFrameDiscardReason::SurfaceLifecycleBreak => {
            DiscardRole::ReleasesHold(HoldReleaseCause::SurfaceLifecycleBreak)
        }
        CachedPresentFrameDiscardReason::RenderFailure => {
            DiscardRole::ReleasesHold(HoldReleaseCause::RenderFailure)
        }
        CachedPresentFrameDiscardReason::WorkerRenderError => {
            DiscardRole::ReleasesHold(HoldReleaseCause::WorkerRenderError)
        }
        // Рендер (или всё окно) уничтожается: кадр чужого рендера рисовать нельзя. Смену это
        // не кончает — при замене рендера (Apply настроек) новое медиа всё ещё придёт.
        CachedPresentFrameDiscardReason::RuntimeDrop => {
            DiscardRole::ReleasesHold(HoldReleaseCause::RendererReplaced)
        }
        CachedPresentFrameDiscardReason::InstalledMediaReleased => {
            DiscardRole::EndsSwitch(MediaSwitchAbandonCause::MediaReleased)
        }
        CachedPresentFrameDiscardReason::PlaylistClearReset => {
            DiscardRole::EndsSwitch(MediaSwitchAbandonCause::MediaReset)
        }
        CachedPresentFrameDiscardReason::PlayerStopped => {
            DiscardRole::EndsSwitch(MediaSwitchAbandonCause::PlayerStopped)
        }
        CachedPresentFrameDiscardReason::PlayerFailed => {
            DiscardRole::EndsSwitch(MediaSwitchAbandonCause::PlayerFailed)
        }
        CachedPresentFrameDiscardReason::PlayerShutdownRequested => {
            DiscardRole::EndsSwitch(MediaSwitchAbandonCause::PlayerShutdown)
        }
        CachedPresentFrameDiscardReason::PlayerFatalError => {
            DiscardRole::EndsSwitch(MediaSwitchAbandonCause::PlayerFatalError)
        }
    }
}

/// Граница открытия нового медиа, которую означает причина сброса кэша (`None` — не граница).
const fn open_boundary_of(reason: CachedPresentFrameDiscardReason) -> Option<OpenBoundary> {
    match reason {
        CachedPresentFrameDiscardReason::MediaOpenBoundary => Some(OpenBoundary::InstallRecorded),
        CachedPresentFrameDiscardReason::PlayerMediaOpened => {
            Some(OpenBoundary::PlayerReportedOpened)
        }
        _ => None,
    }
}

/// Сброс кэша на открытии медиа (запись установки или события плеера об открытии): такой
/// сброс внутри смены не выбрасывает кадр, который уже принадлежит новому медиа.
const fn is_media_open_clear(reason: CachedPresentFrameDiscardReason) -> bool {
    matches!(
        reason,
        CachedPresentFrameDiscardReason::MediaOpenBoundary
            | CachedPresentFrameDiscardReason::PlayerMediaOpenRequested
            | CachedPresentFrameDiscardReason::PlayerMediaOpened
    )
}

/// Есть ли у медиа снимка видеодорожка (`None` — дорожки ещё не известны).
fn snapshot_has_video_track(player_snapshot: &PlayerSnapshot) -> Option<bool> {
    if player_snapshot.tracks.is_empty() {
        return None;
    }
    Some(
        player_snapshot
            .tracks
            .iter()
            .any(|track| track.kind == media_core::TrackKind::Video),
    )
}

impl AppState {
    /// Окно браузерного режима начинает ставить новое медиа: начало замера смены и (по
    /// политике окна) удержания прежнего кадра.
    ///
    /// Поколение рендера берётся из свежего снимка ДО установки: после неё снимок уже несёт
    /// поколение нового медиа, и конец смены нельзя было бы отличить от её начала.
    pub(crate) fn begin_media_switch_frame_watch(&mut self) {
        let policy = self.window_mode.media_switch_frame_policy();
        let from_render_generation = self.refresh_player_snapshot().render_generation;
        let begin = self.media_switch_frame_hold.begin_switch(
            policy,
            from_render_generation,
            Instant::now(),
        );
        debug!(
            ?begin,
            ?policy,
            from_render_generation,
            "Browser window frame hold: media switch began"
        );
    }

    /// Плеер окна сразу отказал ставить новое медиа: прежнее играет, смены нет.
    pub(crate) fn abandon_media_switch_after_refused_load(&mut self) {
        self.abandon_media_switch(MediaSwitchAbandonCause::LoadRefused);
    }

    /// Установка нового медиа не удалась: кадра нового медиа не будет.
    pub(crate) fn abandon_media_switch_after_failed_load(&mut self) {
        self.abandon_media_switch(MediaSwitchAbandonCause::LoadFailed);
    }

    /// Кончает смену без кадра нового медиа и пишет строку замера (если смена шла).
    fn abandon_media_switch(&mut self, cause: MediaSwitchAbandonCause) {
        if let Some(report) = self.media_switch_frame_hold.abandon_switch(cause) {
            log_media_switch_report(&report);
        }
    }

    /// Кэш кадра выбросил кадр (или сбросился без кадра) по `reason`: внутри смены медиа
    /// мягкая причина отдаёт кадр в удержание, жёсткая отпускает удержание или кончает смену.
    pub(in crate::state) fn hand_discarded_frame_to_media_switch(
        &mut self,
        discarded: Option<CachedRenderablePresentFrame>,
        reason: CachedPresentFrameDiscardReason,
    ) {
        let switching = self.media_switch_frame_hold.is_switching();
        if let Some(boundary) = open_boundary_of(reason) {
            self.media_switch_frame_hold.note_open_boundary(boundary);
        }
        match discard_role(reason) {
            DiscardRole::MayHold => {
                let Some(discarded) = discarded else {
                    return;
                };
                // Поздняя граница открытия, когда на экране уже кадр НОВОГО медиа: граница
                // защищает от кадра прежнего медиа, а этот — ровно то, что должно быть на экране.
                // Он остаётся в кэше (без этого следующий показ при пустом слоте — пустое поле).
                if is_media_open_clear(reason)
                    && self
                        .media_switch_frame_hold
                        .keeps_new_media_frame_cached(&discarded.renderable_frame)
                {
                    debug!(
                        ?reason,
                        "Browser window frame hold: the new media frame stays cached"
                    );
                    self.cached_renderable_present_frame = Some(discarded);
                    return;
                }
                let fate = self
                    .media_switch_frame_hold
                    .offer_discarded_frame(discarded.renderable_frame, Instant::now());
                if switching {
                    debug!(
                        ?reason,
                        ?fate,
                        "Browser window frame hold: discarded frame offered"
                    );
                }
            }
            DiscardRole::ReleasesHold(cause) => {
                drop(discarded);
                self.release_media_switch_frame_hold(cause);
            }
            DiscardRole::EndsSwitch(cause) => {
                drop(discarded);
                if switching {
                    debug!(
                        ?reason,
                        "Browser window frame hold: media switch ends without a new frame"
                    );
                }
                self.abandon_media_switch(cause);
            }
        }
    }

    /// Отпускает удержанный кадр по жёсткой причине (смена, если идёт, продолжается).
    pub(in crate::state) fn release_media_switch_frame_hold(&mut self, cause: HoldReleaseCause) {
        let release = self.media_switch_frame_hold.release_hold(cause);
        debug!(?cause, ?release, "Browser window frame hold: release");
    }

    /// Обычный путь выбора кадра остался без видео: кадр, который рисовать вместо пустого
    /// поля (удержанный кадр прежнего медиа), или `None` — рисовать пустое поле.
    ///
    /// Здесь же кончается удержание по сроку и смена на медиа без видео (кадра не будет).
    pub(crate) fn media_switch_frame_for_empty_video(
        &mut self,
        player_snapshot: &PlayerSnapshot,
    ) -> Option<RenderablePresentFrame> {
        let audio_only_media_installed = self
            .media_switch_frame_hold
            .new_media_installed(player_snapshot.render_generation)
            && player_snapshot.media_instance_id.is_some()
            && snapshot_has_video_track(player_snapshot) == Some(false);
        if audio_only_media_installed {
            self.abandon_media_switch(MediaSwitchAbandonCause::AudioOnlyMedia);
            return None;
        }
        self.media_switch_frame_hold
            .held_frame(Instant::now())
            .cloned()
    }

    /// Что покажет кадр окна с этим видеокадром для идущей смены (состояние не меняется).
    pub(crate) fn media_switch_frame_content(
        &self,
        video_frame: Option<&RenderablePresentFrame>,
    ) -> MediaSwitchFrameContent {
        self.media_switch_frame_hold.classify_frame(video_frame)
    }

    /// Кадр окна ушёл на экран: учёт смены; на первом кадре нового медиа — строка замера.
    pub(crate) fn note_media_switch_frame_presented(&mut self, content: MediaSwitchFrameContent) {
        if let Some(report) = self
            .media_switch_frame_hold
            .note_frame_presented(content, Instant::now())
        {
            log_media_switch_report(&report);
        }
    }

    /// Когда кончается удержание по сроку: окно обязано проснуться и нарисовать пустое поле.
    #[must_use]
    pub(crate) fn media_switch_hold_wake_deadline(&self) -> Option<Instant> {
        self.media_switch_frame_hold.hold_deadline()
    }

    /// Каждое пробуждение окна (`decide_frame_work`): вышел срок удержания — кадр отпущен
    /// (`true`: нужен показ, он нарисует пустое поле). Без этого будильник остался бы в
    /// прошлом и будил окно без сна, а пока прежнее медиа ещё играет, запасной путь кадра
    /// (где срок проверяется тоже) не спрашивают вовсе.
    pub(crate) fn expire_due_media_switch_hold(&mut self, now: Instant) -> bool {
        let expired = self.media_switch_frame_hold.expire_due_hold(now) == HoldExpiry::Expired;
        if expired {
            debug!("Browser window frame hold: the hold limit was reached");
        }
        expired
    }
}
