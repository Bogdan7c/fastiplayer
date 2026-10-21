//! Решает, нужна ли полная публикация desktop snapshot-а или хватит обновить позицию.
//!
//! `publish_from_player` вызывается на каждом кадре. Почти всегда меняется только
//! позиция таймлайна; строить для этого заново `DesktopMetadata` со строками, клонировать
//! вид и поднимать revision — работа впустую (D-Bus всё равно молчит про позицию).
//! Здесь живут два маленьких сравнения, которые отделяют «изменилась только позиция»
//! от всего остального, не выделяя памяти.

use desktop_integration::{
    DesktopCapabilities, DesktopControlRevision, DesktopLoopStatus, DesktopPlaybackStatus,
    DesktopSnapshotView, EffectiveVolume,
};
use player_core::PlayerSnapshot;

use super::ExternalPlaybackBinding;

/// Входы построения metadata прошлой полной публикации.
///
/// Пока они равны текущим, строки (`title`, `collection_context`, `source_label`)
/// в `last_snapshot` остаются актуальными и заново не собираются. Всё, что влияет на
/// compound-контекст и title части (структура/metadata очереди), уже входит в
/// `ExternalPlaybackBinding::queue_revision`.
#[derive(Default)]
pub(super) struct MetadataInputs {
    binding: Option<ExternalPlaybackBinding>,
    player_title: Option<String>,
    source_label: Option<String>,
}

impl MetadataInputs {
    /// Запоминает входы (клонирует строки; зовётся только при реальной смене входов).
    pub(super) fn capture(
        binding: Option<ExternalPlaybackBinding>,
        player_snapshot: &PlayerSnapshot,
    ) -> Self {
        // Без binding metadata не строится вовсе: строки плеера не входят в ключ.
        if binding.is_none() {
            return Self::default();
        }
        Self {
            binding,
            player_title: player_snapshot.media_title.clone(),
            source_label: player_snapshot.source_label.clone(),
        }
    }

    /// Сравнивает со снимком по значению, без аллокаций.
    pub(super) fn matches(
        &self,
        binding: Option<ExternalPlaybackBinding>,
        player_snapshot: &PlayerSnapshot,
    ) -> bool {
        if self.binding != binding {
            return false;
        }
        binding.is_none()
            || (self.player_title.as_deref() == player_snapshot.media_title.as_deref()
                && self.source_label.as_deref() == player_snapshot.source_label.as_deref())
    }
}

/// Все не-metadata поля вида, кроме position/revision/seeked. Только `Copy`-данные.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct ScalarSnapshotState {
    pub(super) control_revision: Option<DesktopControlRevision>,
    pub(super) playback_status: DesktopPlaybackStatus,
    pub(super) capabilities: DesktopCapabilities,
    pub(super) loop_status: DesktopLoopStatus,
    pub(super) shuffle: bool,
    pub(super) volume: EffectiveVolume,
}

impl ScalarSnapshotState {
    /// Скалярное состояние уже опубликованного вида.
    pub(super) fn of_view(view: &DesktopSnapshotView) -> Self {
        Self {
            control_revision: view.control_revision,
            playback_status: view.playback_status,
            capabilities: view.capabilities,
            loop_status: view.loop_status,
            shuffle: view.shuffle,
            volume: view.volume,
        }
    }
}
