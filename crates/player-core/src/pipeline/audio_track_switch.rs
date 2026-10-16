//! Pipeline-часть переключения аудиодорожки: атомарная замена audio-слотов.

use super::*;

impl PlaybackPipeline {
    /// Заменяет выбранную аудиодорожку и сбрасывает всё, что принадлежало старой.
    ///
    /// # Что сбрасывается
    /// Decoder (кодек старой дорожки не подходит новым пакетам), output и его clock
    /// (формат вывода пересоздаётся из первого декодированного spec новой дорожки),
    /// очередь ещё не обработанных audio-пакетов (это пакеты старой дорожки),
    /// tempo-процессор и история passthrough (привязаны к формату старого output-а).
    ///
    /// # Что НЕ трогается
    /// Video decoder и очереди кадров, subtitle-состояние, громкость и playback rate
    /// (они живут в session/snapshot и применяются к новому output при его создании).
    ///
    /// Новый decoder создаётся лениво при первом пакете новой дорожки из `new_config`
    /// (`take_deferred_audio_decoder_config` проверяет совпадение `track_id`).
    pub(crate) fn replace_selected_audio_track(
        &mut self,
        new_track_id: TrackId,
        new_config: audio_core::AudioDecoderConfig,
    ) {
        self.clear_audio_decoder();
        self.clear_audio_output();
        self.clear_pending_audio_packets();
        self.clear_passthrough_audio_history();
        self.install_deferred_audio_decoder_config(new_config);
        self.select_audio_track(new_track_id);
    }
}
