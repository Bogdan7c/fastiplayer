//! Восстановление запомненной аудиодорожки до начала воспроизведения нового media.
//!
//! Вызывается из `begin_post_installed_intent` сразу после установки: звук ещё не звучал,
//! поэтому пользователь не слышит «не ту» дорожку. Решение о том, ЧТО выбрать, принимает
//! память выбора (`state/track_memory`); здесь только доставка плееру.
//!
//! Два пути, потому что у стартового восстановления позиции уже есть свой запрос:
//! - есть восстановление позиции (resume) — дорожка идёт ВНУТРИ того же
//!   `InstalledMediaStateRestore` (`InstalledTrackRestore::Select`), порядок «дорожка, потом
//!   позиция» гарантирует плеер;
//! - позиции для восстановления нет — обычная команда `SelectAudioTrack` перед командой
//!   воспроизведения (очередь команд плеера упорядочена).
//!
//! Восстановление из памяти — не выбор пользователя: оно в память не пишется. Любой отказ
//! (очередь команд занята) не мешает открытию: остаётся дорожка по умолчанию.

use super::{ActiveMediaSource, AppState};
use crate::state::track_memory::RememberedAudioPlan;

impl AppState {
    /// Что поставить в `audio_track` запроса восстановления состояния установленного media.
    pub(in crate::state) fn remembered_audio_restore(
        &self,
        source: &ActiveMediaSource,
        player_snapshot: &player_core::PlayerSnapshot,
    ) -> player_core::InstalledTrackRestore {
        match self.remembered_audio_plan(source, player_snapshot) {
            RememberedAudioPlan::Select(track_id) => {
                player_core::InstalledTrackRestore::Select(track_id)
            }
            RememberedAudioPlan::NothingRemembered
            | RememberedAudioPlan::TrackNotFound
            | RememberedAudioPlan::AlreadySelected => {
                player_core::InstalledTrackRestore::KeepDefault
            }
        }
    }

    /// Выбирает запомненную дорожку обычной командой, когда другого запроса восстановления нет.
    pub(super) fn select_remembered_audio_before_playback(
        &mut self,
        source: &ActiveMediaSource,
        player_snapshot: &player_core::PlayerSnapshot,
    ) {
        let RememberedAudioPlan::Select(track_id) =
            self.remembered_audio_plan(source, player_snapshot)
        else {
            return;
        };
        if let Err(error) = self
            .player_worker
            .try_send_command(player_core::PlayerCommand::SelectAudioTrack(track_id))
        {
            tracing::warn!(%error, "Could not restore the remembered audio track; default stays");
        }
    }
}
