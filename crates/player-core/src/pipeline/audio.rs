use super::*;

impl PlaybackPipeline {
    /// Устанавливает codec-neutral audio decoder, созданный session policy слоем.
    pub(crate) fn install_audio_decoder(&mut self, decoder: audio_core::AudioDecoderHandle) {
        self.audio_decoder = Some(decoder);
        self.deferred_audio_decoder_config = None;
    }

    /// Удаляет audio decoder runtime slot без side effects на output/track selection.
    pub(crate) fn clear_audio_decoder(&mut self) {
        self.audio_decoder = None;
    }

    /// Проверяет наличие audio decoder-а без раскрытия `Option` storage.
    #[must_use]
    pub(crate) fn has_audio_decoder(&self) -> bool {
        self.audio_decoder.is_some()
    }

    /// Сохраняет decoder config до первого selected audio packet-а.
    pub(crate) fn install_deferred_audio_decoder_config(
        &mut self,
        config: audio_core::AudioDecoderConfig,
    ) {
        self.deferred_audio_decoder_config = Some(config);
    }

    /// Проверяет наличие deferred decoder config без раскрытия `Option` storage.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_deferred_audio_decoder_config(&self) -> bool {
        self.deferred_audio_decoder_config.is_some()
    }

    /// Забирает deferred decoder config только для выбранного track-а.
    pub(crate) fn take_deferred_audio_decoder_config(
        &mut self,
        track_id: TrackId,
    ) -> Option<audio_core::AudioDecoderConfig> {
        let config = self.deferred_audio_decoder_config.as_ref()?;
        if config.track_id() != track_id.get() {
            return None;
        }

        self.deferred_audio_decoder_config.take()
    }

    /// Удаляет deferred decoder config при сбросе media или отключении audio path.
    pub(crate) fn clear_deferred_audio_decoder_config(&mut self) {
        self.deferred_audio_decoder_config = None;
    }

    /// Декодирует audio packet через установленный decoder.
    ///
    /// `None` означает absent decoder и сохраняет прежний no-op path. `Err`
    /// остаётся отдельным состоянием, чтобы session могла выставить runtime error.
    pub(crate) fn decode_audio_packet(
        &mut self,
        encoded_audio_packet: &audio_core::EncodedAudioPacket<'_>,
    ) -> Option<anyhow::Result<DecodedAudioPacket>> {
        let decoder = self.audio_decoder.as_mut()?;
        let decode_result = decoder.decode(encoded_audio_packet);
        Some(decode_result.map(|samples| DecodedAudioPacket {
            samples,
            sample_rate: decoder.sample_rate(),
            channels: decoder.channels(),
        }))
    }

    /// Сбрасывает codec state установленного audio decoder-а после seek/discontinuity.
    ///
    /// `None` означает absent decoder, а `Some(Err(_))` сохраняет reset error для session.
    pub(crate) fn reset_audio_decoder(&mut self) -> Option<anyhow::Result<()>> {
        self.audio_decoder.as_mut().map(|decoder| decoder.reset())
    }

    /// Устанавливает audio output, созданный composition/session boundary.
    pub(crate) fn install_audio_output(&mut self, output: Box<dyn PlayerAudioOutput>) {
        self.audio_output = Some(output);
        self.audio_output_play_requested = false;
    }

    /// Устанавливает fake/stub output для unit-тестов без CPAL device side effects.
    #[cfg(test)]
    pub(crate) fn install_audio_output_for_tests(&mut self, output: Box<dyn PlayerAudioOutput>) {
        self.audio_output = Some(output);
        self.audio_output_play_requested = false;
    }

    /// Удаляет audio output runtime slot без изменения decoder/track selection.
    pub(crate) fn clear_audio_output(&mut self) {
        self.audio_output = None;
        self.clear_audio_tempo_processor();
        self.audio_output_play_requested = false;
    }

    /// Проверяет наличие audio output-а без доступа к concrete CPAL handle.
    #[must_use]
    pub(crate) fn has_audio_output(&self) -> bool {
        self.audio_output.is_some()
    }

    /// Записывает samples в output ring buffer, если output установлен.
    ///
    /// Возвращает `None` для absent output и `Some(written_samples)` для активного output.
    pub(crate) fn write_audio_output_samples(&mut self, samples: &[f32]) -> Option<u64> {
        self.audio_output
            .as_mut()
            .map(|output| output.write_samples(samples))
    }

    /// Устанавливает tempo processor, которым pipeline владеет между decoder и output.
    pub(crate) fn install_audio_tempo_processor(
        &mut self,
        processor: AudioTempoProcessorHandle,
        pcm_format: AudioTempoPcmFormat,
    ) {
        self.audio_tempo_processor = Some(processor);
        self.audio_tempo_pcm_format = Some(pcm_format);
    }

    /// Очищает tempo processor так, чтобы `1.0x` снова был прямым PCM passthrough.
    pub(crate) fn clear_audio_tempo_processor(&mut self) {
        self.audio_tempo_processor = None;
        self.audio_tempo_pcm_format = None;
    }

    /// Проверяет, активен ли tempo processor для non-1x audio path.
    #[must_use]
    pub(crate) fn has_audio_tempo_processor(&self) -> bool {
        self.audio_tempo_processor.is_some()
    }

    /// Возвращает PCM format активного tempo processor-а без доступа к processor storage.
    #[must_use]
    pub(crate) const fn audio_tempo_pcm_format(&self) -> Option<AudioTempoPcmFormat> {
        self.audio_tempo_pcm_format
    }

    /// Создаёт новый neutral tempo segment id для accepted playback-rate boundary.
    pub(crate) fn next_audio_tempo_segment(
        &mut self,
        playback_rate: PlaybackRate,
    ) -> anyhow::Result<AudioTempoSegment> {
        let segment_id = AudioTempoSegmentId::new(self.next_audio_tempo_segment_id);
        self.next_audio_tempo_segment_id = self.next_audio_tempo_segment_id.saturating_add(1);
        let ratio = AudioTempoRatio::new(f64::from(playback_rate.as_f32()))?;
        Ok(AudioTempoSegment::new(segment_id, ratio))
    }

    /// Меняет tempo segment внутри активного processor-а; absent processor остаётся no-op.
    pub(crate) fn set_audio_tempo_segment(
        &mut self,
        segment: AudioTempoSegment,
    ) -> Option<anyhow::Result<AudioTempoProcessReport>> {
        self.audio_tempo_processor
            .as_mut()
            .map(|processor| processor.set_segment(segment))
    }

    /// Обрабатывает decoded PCM через активный tempo processor.
    pub(crate) fn process_audio_tempo_samples(
        &mut self,
        samples: &[f32],
    ) -> Option<anyhow::Result<AudioTempoStretchedOutput>> {
        let processor = self.audio_tempo_processor.as_mut()?;
        let Some(pcm_format) = self.audio_tempo_pcm_format else {
            return Some(Err(anyhow::anyhow!(
                "audio tempo processor is installed without its PCM format"
            )));
        };
        let decoded_media =
            match AudioTempoDecodedMedia::from_interleaved_samples(samples, pcm_format) {
                Ok(decoded_media) => decoded_media,
                Err(error) => return Some(Err(error)),
            };

        Some(processor.process_decoded_media(decoded_media))
    }

    /// Дренирует pending output tempo processor-а один раз и очищает processor slot.
    pub(crate) fn flush_and_clear_audio_tempo_processor(
        &mut self,
    ) -> Option<anyhow::Result<AudioTempoStretchedOutput>> {
        let mut processor = self.audio_tempo_processor.take()?;
        self.audio_tempo_pcm_format = None;
        Some(processor.flush())
    }

    /// Запускает audio output stream без смешивания absent output и CPAL error.
    pub(crate) fn play_audio_output(&mut self) -> Option<anyhow::Result<()>> {
        let output = self.audio_output.as_mut()?;
        let play_result = output.play();
        if play_result.is_ok() {
            self.audio_output_play_requested = true;
        }
        Some(play_result)
    }

    /// Ставит audio output stream на паузу без изменения high-level playback state.
    pub(crate) fn pause_audio_output(&mut self) -> Option<anyhow::Result<()>> {
        let output = self.audio_output.as_mut()?;
        let pause_result = output.pause();
        if pause_result.is_ok() {
            self.audio_output_play_requested = false;
        }
        Some(pause_result)
    }

    /// Очищает audio output buffer для seek и возвращает sync ack generation.
    pub(crate) fn clear_audio_output_for_seek(
        &mut self,
        generation: u64,
    ) -> Option<anyhow::Result<u64>> {
        let clear_result = self
            .audio_output
            .as_mut()
            .map(|output| output.clear_buffer_for_seek(generation));
        self.clear_audio_tempo_processor();
        clear_result
    }

    /// Устанавливает громкость output-а, если runtime output уже существует.
    pub(crate) fn set_audio_output_volume(&mut self, volume: f32) -> bool {
        let Some(output) = self.audio_output.as_mut() else {
            return false;
        };

        output.set_volume(volume);
        true
    }

    /// Возвращает уровень audio buffer через output boundary.
    #[must_use]
    pub(crate) fn audio_output_buffer_level_ms(&self) -> Option<f64> {
        self.audio_output
            .as_ref()
            .map(|output| output.buffer_level_ms())
    }

    /// Возвращает EOF-drain состояние audio tail-а без доступа к queue/output storage.
    #[must_use]
    pub(crate) fn audio_eof_drain_state(&self) -> AudioEofDrainState {
        if self.audio_track_id.is_none() {
            return AudioEofDrainState::NoSelectedAudio;
        }

        let queued_packets = self.pending_audio_packets.len();
        if queued_packets > 0 {
            return AudioEofDrainState::PendingPackets { queued_packets };
        }

        let Some(output) = self.audio_output.as_ref() else {
            return AudioEofDrainState::NoOutput;
        };

        let buffer_level_ms = output.buffer_level_ms();
        if buffer_level_ms.is_finite() && buffer_level_ms > 0.0 {
            return AudioEofDrainState::DrainingOutput {
                buffer_level_ms,
                playback_requested: self.audio_output_play_requested,
            };
        }

        AudioEofDrainState::DrainedOutput {
            playback_requested: self.audio_output_play_requested,
        }
    }

    /// Возвращает clock handle output-а без раскрытия самого output slot-а.
    #[must_use]
    pub(crate) fn audio_output_clock(&self) -> Option<Arc<dyn PlayerAudioClock>> {
        self.audio_output.as_ref().map(|output| output.clock())
    }

    /// Проверяет наличие audio clock без раскрытия `Option` storage.
    #[must_use]
    pub(crate) fn has_audio_clock(&self) -> bool {
        self.audio_clock.is_some()
    }

    /// Устанавливает audio clock handle и отключает no-audio monotonic fallback.
    pub(crate) fn install_audio_clock(&mut self, clock: Arc<dyn PlayerAudioClock>) {
        let output_clock_position = clock.now();
        self.audio_clock = Some(clock);
        self.audio_clock_media_mapping_anchor = AudioClockMediaMappingAnchor::new(
            self.media_clock_base,
            output_clock_position,
            self.audio_clock_media_mapping_anchor.playback_rate,
        );
        self.clear_monotonic_media_clock();
    }

    /// Удаляет audio clock handle без изменения decoder/output slots.
    pub(crate) fn clear_audio_clock(&mut self) {
        self.audio_clock = None;
    }

    /// Сбрасывает установленный audio clock; absent clock остаётся явным no-op.
    pub(crate) fn reset_audio_clock(&mut self) -> bool {
        let Some(clock) = self.audio_clock.as_ref() else {
            return false;
        };

        clock.reset();
        self.audio_clock_media_mapping_anchor = AudioClockMediaMappingAnchor::new(
            self.audio_clock_media_mapping_anchor.media_position,
            Duration::ZERO,
            self.audio_clock_media_mapping_anchor.playback_rate,
        );
        true
    }

    /// Возвращает текущее audio clock time или `Duration::ZERO`, если clock отсутствует.
    #[must_use]
    pub(crate) fn audio_clock_now(&self) -> Duration {
        self.audio_clock
            .as_ref()
            .map(|clock| clock.now())
            .unwrap_or(Duration::ZERO)
    }

    /// Возвращает число audio underrun callbacks для snapshot diagnostics.
    #[must_use]
    pub(crate) fn audio_clock_underrun_callbacks(&self) -> u64 {
        self.audio_clock
            .as_ref()
            .map(|clock| clock.underrun_callbacks())
            .unwrap_or(0)
    }

    /// Возвращает media base для seek/preroll trimming.
    #[must_use]
    pub(crate) const fn media_clock_base(&self) -> Duration {
        self.media_clock_base
    }

    /// Устанавливает media base и перепривязывает output-clock mapping к текущему clock.
    pub(crate) fn set_media_clock_base(&mut self, position: Duration) {
        self.reanchor_audio_clock_media_mapping(
            position,
            self.audio_clock_media_mapping_anchor.playback_rate,
        );
    }

    /// Перепривязывает media position к текущему output clock и playback rate.
    pub(crate) fn reanchor_audio_clock_media_mapping(
        &mut self,
        media_position: Duration,
        playback_rate: PlaybackRate,
    ) {
        self.media_clock_base = media_position;
        self.audio_clock_media_mapping_anchor = AudioClockMediaMappingAnchor::new(
            media_position,
            self.audio_clock_now(),
            playback_rate,
        );
    }

    /// Возвращает absolute media position, mapped from output-clock progress.
    #[must_use]
    pub(crate) fn media_position_from_audio_clock(&self) -> Duration {
        self.audio_clock_media_mapping_anchor
            .media_position_at_output_clock(self.audio_clock_now())
    }

    /// Запускает monotonic fallback clock от заданной media position и playback rate.
    pub(crate) fn start_monotonic_media_clock(
        &mut self,
        position: Duration,
        now: Instant,
        playback_rate: PlaybackRate,
    ) {
        self.monotonic_media_clock_anchor =
            Some(MonotonicMediaClockAnchor::new(position, now, playback_rate));
    }

    /// Очищает monotonic fallback clock для pause/seek/audio-clock paths.
    pub(crate) fn clear_monotonic_media_clock(&mut self) {
        self.monotonic_media_clock_anchor = None;
    }

    /// Возвращает no-audio fallback media position, если fallback anchor активен.
    #[must_use]
    pub(crate) fn monotonic_media_position(&self, now: Instant) -> Option<Duration> {
        self.monotonic_media_clock_anchor
            .map(|anchor| anchor.position_at(now))
    }

    /// Обновляет sample для stall detection только при реальном движении audio clock.
    pub(crate) fn note_audio_clock_sample(&mut self, audio_now: Duration, observed_at: Instant) {
        if audio_now == self.last_audio_clock {
            return;
        }

        self.last_audio_clock = audio_now;
        self.last_audio_clock_change_at = observed_at;
    }

    /// Переставляет baseline stall detection после play/autoplay/seek resume.
    pub(crate) fn reset_audio_clock_sample(&mut self, audio_now: Duration, observed_at: Instant) {
        self.last_audio_clock = audio_now;
        self.last_audio_clock_change_at = observed_at;
    }

    /// Возвращает длительность, в течение которой audio clock не менял значение.
    #[must_use]
    pub(crate) fn audio_clock_stalled_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_audio_clock_change_at)
    }

    /// Возвращает последнее поколение, для которого audio buffer clear был подтверждён.
    #[must_use]
    pub(crate) const fn audio_buffer_clear_generation(&self) -> u64 {
        self.audio_buffer_clear_generation
    }

    /// Отмечает синхронное подтверждение очистки audio buffer для seek generation.
    pub(crate) fn mark_audio_buffer_clear_ack(&mut self, generation: u64) {
        self.audio_buffer_clear_generation = generation;
    }
}
