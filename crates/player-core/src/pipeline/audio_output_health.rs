//! Здоровье backend stream-а активного audio output-а.
//!
//! Отдельный модуль, потому что `pipeline/audio.rs` упирается в предел размера модуля.
//! Pipeline только сообщает факт; решение о восстановлении принимает session
//! (`session::audio_output_recovery`).

use audio_core::AudioOutputStreamHealth;

use super::PlaybackPipeline;

impl PlaybackPipeline {
    /// Возвращает здоровье активного output stream-а.
    ///
    /// `None` — output-а нет (ещё не создан, отключён или media без звука); это не то же
    /// самое, что «поток сломан», и вызывающий код не должен их смешивать.
    #[must_use]
    pub(crate) fn audio_output_stream_health(&self) -> Option<AudioOutputStreamHealth> {
        self.audio_output
            .as_ref()
            .map(|audio_output| audio_output.stream_health())
    }
}
