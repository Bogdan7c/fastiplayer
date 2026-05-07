/// Высокоуровневое состояние воспроизведения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// Media ещё не выбран или session полностью очищена.
    Idle,

    /// Player принимает источник и готовит pipeline.
    Opening,

    /// Воспроизведение готово, но сейчас остановлено пользователем.
    Paused,

    /// Media активно воспроизводится.
    Playing,

    /// Player ждёт достаточного количества audio/video данных.
    Buffering,

    /// Выполняется seek и пересборка очередей.
    Seeking,

    /// EOF уже достигнут, но pipeline ещё дорендеривает buffered data.
    Draining,

    /// Media полностью закончился.
    Ended,

    /// Session остановлена через shutdown.
    Stopped,

    /// Текущий media или runtime pipeline перешёл в fatal error.
    Failed,
}

impl Default for PlaybackState {
    /// Новый player session стартует без выбранного media.
    fn default() -> Self {
        Self::Idle
    }
}

impl PlaybackState {
    /// Возвращает `true`, если state machine должна продвигать playback.
    #[must_use]
    pub const fn is_playback_active(self) -> bool {
        matches!(
            self,
            Self::Playing | Self::Buffering | Self::Seeking | Self::Draining
        )
    }
}
