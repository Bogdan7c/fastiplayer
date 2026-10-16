//! Выбор субтитров на границе player-core: проверка id против дорожек установленного media.
//!
//! # Границы
//! Player-core знает только дорожки, которые отдал демуксер (`media_core::TrackInfo`).
//! Внешние (sidecar) субтитры живут в приложении и сюда НЕ попадают: у них нет `TrackId`
//! этого media. Сегодня `TrackKind` содержит только `Video` и `Audio`, поэтому ни один
//! `Some(id)` не может быть валидным subtitle-треком. Когда в `TrackKind` появится вариант
//! субтитров, компилятор заставит обновить [`track_kind_is_subtitle`], и проверка начнёт
//! пропускать встроенные дорожки без других правок.
//!
//! # Семантика
//! - `None` (выключить) допустим всегда: это не ссылка на дорожку.
//! - `Some(id)` принимается, только если у текущего media есть subtitle-трек с таким id.
//! - Отказ — `PlayerErrorKind::InvalidCommand` (как у остальных runtime-команд с плохим
//!   входом): выбор в snapshot не меняется, `SubtitleTrackSelected` не публикуется.
//! - Восстановление после установки media (`InstalledSubtitleRestore::Select`) НЕ имеет
//!   права ронять установку: id из прошлой сессии может не существовать → деградация в
//!   «без субтитров» (см. [`PlayerSession::subtitle_restore_target_is_known`]).

use media_core::{TrackId, TrackKind};

use super::PlayerSession;
use crate::{PlayerError, PlayerErrorKind, PlayerResult};

/// Субтитровая ли дорожка этого вида. Исчерпывающий `match` намеренно без `_`.
const fn track_kind_is_subtitle(kind: TrackKind) -> bool {
    match kind {
        TrackKind::Subtitle => true,
        TrackKind::Video | TrackKind::Audio => false,
    }
}

impl PlayerSession {
    /// Есть ли у текущего media subtitle-трек с таким id (нет media — нет и трека).
    pub(super) fn has_subtitle_track(&self, track_id: TrackId) -> bool {
        self.pipeline
            .tracks()
            .iter()
            .any(|track| track.id == track_id && track_kind_is_subtitle(track.kind))
    }

    /// Проверяет `Some(id)` для команды `SelectSubtitleTrack`; `None` проходит всегда.
    ///
    /// Отказ записывается как recoverable error (как у других команд), состояние выбора
    /// при этом не трогается — это обязанность вызывающего: он вызывает проверку ДО записи.
    pub(super) fn validate_subtitle_track_selection(
        &mut self,
        track_id: Option<TrackId>,
    ) -> PlayerResult<()> {
        let Some(track_id) = track_id else {
            return Ok(());
        };
        if self.has_subtitle_track(track_id) {
            return Ok(());
        }
        let error = PlayerError::new(
            PlayerErrorKind::InvalidCommand,
            format!("subtitle track {track_id:?} does not exist in the current media"),
        );
        self.record_recoverable_error(error.clone());
        Err(error)
    }

    /// Можно ли применить сохранённый выбор при восстановлении состояния после установки.
    ///
    /// Неизвестный id (трек из прошлой сессии) — не ошибка установки: вызывающий пропускает
    /// выбор, и субтитры остаются как есть («без субтитров»). `None` применим всегда.
    pub(super) fn subtitle_restore_target_is_known(&self, track_id: Option<TrackId>) -> bool {
        let Some(track_id) = track_id else {
            return true;
        };
        let known = self.has_subtitle_track(track_id);
        if !known {
            tracing::debug!(
                ?track_id,
                "Subtitle track from the saved state is not in this media; keeping subtitles off"
            );
        }
        known
    }
}
