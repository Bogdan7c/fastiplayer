//! Владелец и встроенный PGS: приём пакетов, перенос изменений на шкалу дорожки и политика
//! памяти по ролям (показываемая / фоновая).
//!
//! Логика состояния (декодер, хранилище, связь с событиями шкалы) — в [`super::pgs_tracks`];
//! здесь только то, что требует `SubtitleOwner`: список дорожек, презентатор, выдача новых
//! идентичностей источника, счётчики и лог.
//!
//! # Откуда берётся «сейчас» для политики памяти
//! Из наблюдения за плеером (`SubtitlePresenter::position_for_retention`): позиция media
//! (в seek/scrub — цель), сдвинутая на синхронизацию субтитров. Это позиция плеера, а не
//! последний показанный кадр субтитров: при выключенных субтитрах кадры в презентатор не
//! приходят, а фоновые дорожки всё равно обязаны держать только свой хвост. Пока позиция
//! неизвестна (нет ни одного снимка), политика не применяется.
//!
//! # Когда выполняется работа
//! Только по событиям, без таймеров и без работы на кадре, когда ничего не менялось:
//! - после пачки пакетов — для дорожек, получивших пакеты;
//! - при смене показываемой дорожки (меню, горячая клавиша, автовыбор, переключение
//!   «субтитры включены/выключены») — для всех PGS-дорожек media (их единицы). Проверка смены —
//!   сравнение одного идентификатора за вызов ([`SubtitleOwner::apply_pgs_roles`]).

use player_core::{SubtitleFeedPacket, SubtitleReadRun};
use subtitle_core::SubtitleTrackId;
use subtitle_pgs::PgsRetention;

use super::pgs_tracks::{
    ForcedPicturesView, PgsBatches, PgsPendingChanges, PgsTrackRole, ReadRunAdmission,
    TimelineChange,
};
use crate::state::subtitles::limits::PgsLimits;
use crate::state::subtitles::owner_embedded::EmbeddedUpdate;
use crate::state::subtitles::track_list::TrackLoadState;
use crate::state::subtitles::{CurrentMediaForSubtitles, SubtitleOwner};

impl SubtitleOwner {
    /// Принимает PGS-пакет дорожки `track_id`: декодирует и накапливает изменения в `batches`.
    /// Шкала при этом не трогается — изменения применит [`Self::apply_pgs_batches`].
    pub(super) fn accept_pgs_packet(
        &mut self,
        track_id: SubtitleTrackId,
        packet: &SubtitleFeedPacket,
        batches: &mut PgsBatches,
    ) {
        let decoder_limits = self.settings.embedded_limits.pgs.decoder;
        let Some(media) = self.media.as_mut() else {
            return;
        };
        // Сначала проход чтения: новый — разрыв для ВСЕХ PGS-дорожек media, старый (хвост
        // трубы от времени до seek) — пакет выбрасывается, иначе он воссоздал бы открытую
        // композицию, которой после seek уже нет.
        if media
            .embedded_pgs
            .admit_packet_read_run(packet.read_run, batches)
            == ReadRunAdmission::Stale
        {
            return;
        }
        // Вид «только forced» создаётся вместе с состоянием дорожки: ему нужны общий сдвиг media,
        // язык дорожки и собственная идентичность источника.
        let (state, counters) =
            media
                .embedded_pgs
                .track_or_create(track_id, decoder_limits, || {
                    let language = match media.tracks.get(track_id).map(|entry| &entry.load) {
                        Some(TrackLoadState::Ready(track)) => track.language().cloned(),
                        _ => None,
                    };
                    ForcedPicturesView::new(self.source_ids.allocate(), media.sync_offset, language)
                });
        state.ingest_packet(packet, batches.entry(track_id).or_default(), counters);
    }

    /// Принимает проход чтения демуксера из снимка плеера. Если он новее известного — это
    /// разрыв потока (seek/scrub/потеря пакета), и PGS-дорожки media сбрасываются СРАЗУ, не
    /// дожидаясь следующего пакета: иначе открытая композиция до seek висела бы на экране,
    /// пока дорожка не пришлёт новый пакет (а он может не прийти до следующей реплики).
    /// Наблюдение относится к `current` media; чужое media игнорируется.
    pub(in crate::state::subtitles) fn observe_pgs_read_run(
        &mut self,
        current: CurrentMediaForSubtitles,
        run: SubtitleReadRun,
    ) -> EmbeddedUpdate {
        let Some(media) = self.media.as_mut() else {
            return EmbeddedUpdate::Unchanged;
        };
        if current.media_instance_id != Some(media.media_instance_id) {
            return EmbeddedUpdate::Unchanged;
        }
        let mut batches = PgsBatches::new();
        // Устаревшее наблюдение ничего не меняет и в счётчики пакетов не идёт.
        let _admission = media.embedded_pgs.observe_read_run(run, &mut batches);
        self.apply_pgs_batches(batches)
    }

    /// Переносит накопленные изменения на шкалы дорожек (не больше одной пересборки на дорожку)
    /// и применяет политику памяти. Изменилось ли то, что видит пользователь.
    pub(super) fn apply_pgs_batches(&mut self, batches: PgsBatches) -> EmbeddedUpdate {
        let mut update = EmbeddedUpdate::Unchanged;
        for (track_id, pending) in batches {
            update = update.merge(self.settle_pgs_track(track_id, pending));
        }
        update
    }

    /// Применяет роли после возможной смены показываемой дорожки: у дорожек, сменивших роль,
    /// политика памяти пересчитывается (показываемая получает бюджет, бывшая показываемая
    /// сжимается до хвоста). Текущая картинка при этом сохраняется: политика не вытесняет
    /// композицию, активную в позиции воспроизведения.
    pub(super) fn apply_pgs_roles(&mut self) -> EmbeddedUpdate {
        let Some(media) = self.media.as_mut() else {
            return EmbeddedUpdate::Unchanged;
        };
        let displayed = media.display_track_id();
        if !media.embedded_pgs.note_displayed_track(displayed) {
            return EmbeddedUpdate::Unchanged;
        }
        let mut update = EmbeddedUpdate::Unchanged;
        for track_id in media.embedded_pgs.known_tracks() {
            update = update.merge(self.settle_pgs_track(track_id, PgsPendingChanges::default()));
        }
        update
    }

    /// Применяет к дорожке накопленные изменения и политику памяти ее роли. Дорожки без PGS-
    /// состояния и не готовые к показу пропускаются.
    fn settle_pgs_track(
        &mut self,
        track_id: SubtitleTrackId,
        pending: PgsPendingChanges,
    ) -> EmbeddedUpdate {
        let limits: PgsLimits = self.settings.embedded_limits.pgs;
        let Some(media) = self.media.as_mut() else {
            return EmbeddedUpdate::Unchanged;
        };
        let role = if media.display_track_id() == Some(track_id) {
            PgsTrackRole::Displayed
        } else {
            PgsTrackRole::Background
        };
        let position = self.presenter.position_for_retention(media.sync_offset);
        let Some((state, counters)) = media.embedded_pgs.existing_track(track_id) else {
            return EmbeddedUpdate::Unchanged;
        };
        let Some(TrackLoadState::Ready(track)) =
            media.tracks.get_mut(track_id).map(|entry| &mut entry.load)
        else {
            return EmbeddedUpdate::Unchanged;
        };

        // Сначала память: хранилище уже содержит все новые композиции, поэтому вытеснение
        // видит итоговое состояние, а шкала пересобирается один раз за обе причины.
        let evicted_from_timeline = position.is_some_and(|position| {
            let policy = retention_policy_for(role, &limits, state.estimated_bytes());
            state
                .enforce_retention(position, policy, counters)
                .requires_rebuild()
        });
        let change =
            state.apply_to_timeline(pending, evicted_from_timeline, track, counters, || {
                self.source_ids.allocate()
            });
        match change {
            TimelineChange::Untouched => EmbeddedUpdate::Unchanged,
            TimelineChange::Edited => visible_if(role),
            TimelineChange::Rebuilt => {
                tracing::debug!(
                    ?role,
                    "A PGS track timeline was rebuilt from its composition store"
                );
                // Новый источник: показанная картинка прежнего источника больше не верна.
                if role == PgsTrackRole::Displayed {
                    self.presenter.forget_shown_overlay();
                }
                visible_if(role)
            }
        }
    }
}

/// Политика удержания роли.
fn retention_policy_for(
    role: PgsTrackRole,
    limits: &PgsLimits,
    stored_bytes: usize,
) -> PgsRetention {
    match role {
        PgsTrackRole::Displayed => limits.shown_retention(stored_bytes),
        PgsTrackRole::Background => limits.background_retention(),
    }
}

/// Изменение шкалы видно пользователю только у показываемой дорожки.
fn visible_if(role: PgsTrackRole) -> EmbeddedUpdate {
    match role {
        PgsTrackRole::Displayed => EmbeddedUpdate::Changed,
        PgsTrackRole::Background => EmbeddedUpdate::Unchanged,
    }
}
