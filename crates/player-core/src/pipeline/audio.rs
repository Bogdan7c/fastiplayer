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
        Some(decode_result.and_then(|samples| {
            if samples.is_empty() {
                return Ok(DecodedAudioPacket::Empty);
            }

            let output_spec = decoder.decoded_output_spec().ok_or_else(|| {
                anyhow::anyhow!(
                    "Audio decoder produced PCM without a complete sample-rate/channel-layout spec"
                )
            })?;
            Ok(DecodedAudioPacket::Pcm {
                samples,
                output_spec,
            })
        }))
    }

    /// Сбрасывает codec state установленного audio decoder-а после seek/discontinuity.
    ///
    /// `None` означает absent decoder, а `Some(Err(_))` сохраняет reset error для session.
    pub(crate) fn reset_audio_decoder(&mut self) -> Option<anyhow::Result<()>> {
        self.audio_decoder.as_mut().map(|decoder| decoder.reset())
    }

    /// Устанавливает audio output, созданный composition/session boundary.
    pub(crate) fn install_audio_output(
        &mut self,
        output: Box<dyn PlayerAudioOutput>,
        input_spec: audio_core::AudioOutputSpec,
    ) {
        self.audio_output = Some(output);
        self.audio_output_input_spec = Some(input_spec);
        self.audio_output_play_requested = false;
    }

    /// Возвращает spec, который принял установленный production output.
    #[must_use]
    pub(crate) const fn audio_output_input_spec(&self) -> Option<audio_core::AudioOutputSpec> {
        self.audio_output_input_spec
    }

    /// Устанавливает fake/stub output для unit-тестов без CPAL device side effects.
    #[cfg(test)]
    pub(crate) fn install_audio_output_for_tests(&mut self, output: Box<dyn PlayerAudioOutput>) {
        self.audio_output = Some(output);
        self.audio_output_input_spec = None;
        self.audio_output_play_requested = false;
    }

    /// Удаляет audio output и принадлежащий ему clock без изменения decoder/track selection.
    pub(crate) fn clear_audio_output(&mut self) {
        self.audio_output = None;
        self.audio_output_input_spec = None;
        self.clear_audio_tempo_processor();
        self.clear_audio_clock();
        self.audio_output_play_requested = false;
    }

    /// Проверяет наличие audio output-а без доступа к concrete CPAL handle.
    #[must_use]
    pub(crate) fn has_audio_output(&self) -> bool {
        self.audio_output.is_some()
    }

    /// Записывает samples в output и сохраняет typed absent/error/frame accounting.
    pub(crate) fn write_audio_output_samples(
        &mut self,
        samples: &[f32],
        intent: audio_core::AudioOutputWriteIntent,
    ) -> AudioOutputRoutingStatus {
        route_audio_output_samples(&mut self.audio_output, samples, intent)
    }

    /// Устанавливает tempo processor, которым pipeline владеет между decoder и output.
    pub(crate) fn install_audio_tempo_processor(&mut self, processor: AudioTempoProcessorHandle) {
        self.audio_tempo_processor = Some(processor);
    }

    /// Очищает tempo processor так, чтобы `1.0x` снова был прямым PCM passthrough.
    ///
    /// Warmup история чистится вместе с processor-ом: этот путь проходит через
    /// seek/media discontinuity, и PCM до разрыва не должен праймить processor
    /// после него.
    pub(crate) fn clear_audio_tempo_processor(&mut self) {
        self.audio_tempo_processor = None;
        self.audio_tempo_output_buffer.clear();
        self.clear_passthrough_audio_history();
    }

    /// Проверяет, активен ли tempo processor для non-1x audio path.
    #[must_use]
    pub(crate) fn has_audio_tempo_processor(&self) -> bool {
        self.audio_tempo_processor.is_some()
    }

    /// Возвращает PCM format активного tempo processor-а без доступа к processor storage.
    #[must_use]
    pub(crate) fn audio_tempo_pcm_format(&self) -> Option<AudioTempoPcmFormat> {
        self.audio_tempo_processor
            .as_ref()
            .map(|processor| processor.pcm_format())
    }

    /// Готовит neutral tempo segment, не меняя lifecycle state до подтверждения backend-а.
    pub(crate) fn propose_audio_tempo_segment(
        &self,
        playback_rate: PlaybackRate,
    ) -> anyhow::Result<AudioTempoSegment> {
        let segment_id = AudioTempoSegmentId::new(self.next_audio_tempo_segment_id);
        let ratio = AudioTempoRatio::new(f64::from(playback_rate.as_f32()))?;
        Ok(AudioTempoSegment::new(segment_id, ratio))
    }

    /// Подтверждает id только после успешного создания/переключения tempo processor-а.
    pub(crate) fn commit_audio_tempo_segment(&mut self, segment: AudioTempoSegment) {
        debug_assert_eq!(
            segment.segment_id().get(),
            self.next_audio_tempo_segment_id,
            "commit должен соответствовать последнему предложенному tempo segment"
        );
        self.next_audio_tempo_segment_id = self.next_audio_tempo_segment_id.saturating_add(1);
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

    /// Обрабатывает decoded PCM и маршрутизирует output по явному intent-у.
    pub(crate) fn process_audio_tempo_samples(
        &mut self,
        samples: &[f32],
        pcm_format: AudioTempoPcmFormat,
    ) -> Option<anyhow::Result<RoutedAudioTempoOutput>> {
        let processor = self.audio_tempo_processor.as_mut()?;
        let decoded_media =
            match AudioTempoDecodedMedia::from_interleaved_samples(samples, pcm_format) {
                Ok(decoded_media) => decoded_media,
                Err(error) => return Some(Err(error)),
            };

        let stretched_output = match processor
            .process_decoded_media_into(decoded_media, &mut self.audio_tempo_output_buffer)
        {
            Ok(stretched_output) => stretched_output,
            Err(error) => return Some(Err(error)),
        };
        let report = stretched_output.report().clone();
        let write_status = route_audio_output_samples(
            &mut self.audio_output,
            stretched_output.interleaved_samples(),
            audio_core::AudioOutputWriteIntent::TempoProcessed,
        );

        Some(Ok(RoutedAudioTempoOutput {
            report,
            write_status,
        }))
    }

    /// Праймит processor position-free историей без produced/pending PCM.
    pub(crate) fn prime_audio_tempo_history(
        &mut self,
        samples: &[f32],
        pcm_format: AudioTempoPcmFormat,
    ) -> Option<anyhow::Result<AudioTempoProcessReport>> {
        let processor = self.audio_tempo_processor.as_mut()?;
        let decoded_history =
            match AudioTempoDecodedMedia::from_interleaved_samples(samples, pcm_format) {
                Ok(decoded_history) => decoded_history,
                Err(error) => return Some(Err(error)),
            };

        Some(processor.prime_decoded_history(decoded_history))
    }

    /// Записывает passthrough decoded PCM в bounded warmup историю.
    ///
    /// История нужна только пока tempo processor отсутствует: свежий processor
    /// праймится ею, чтобы не стартовать phase vocoder с пустого окна.
    pub(crate) fn record_passthrough_audio_history(
        &mut self,
        samples: &[f32],
        output_spec: audio_core::AudioOutputSpec,
    ) {
        let sample_rate = output_spec.sample_rate;
        let channels = output_spec.channels();
        if sample_rate == 0 || channels == 0 || samples.is_empty() {
            return;
        }

        if self.passthrough_audio_history_spec != Some(output_spec) {
            self.passthrough_audio_history.clear();
            self.passthrough_audio_history_spec = Some(output_spec);
        }

        self.passthrough_audio_history.extend_from_slice(samples);

        let max_samples = passthrough_audio_history_max_samples(sample_rate, channels);
        let overflow = self
            .passthrough_audio_history
            .len()
            .saturating_sub(max_samples);
        if overflow > 0 {
            let frame_aligned_overflow = overflow.next_multiple_of(channels as usize);
            let drain_end = frame_aligned_overflow.min(self.passthrough_audio_history.len());
            self.passthrough_audio_history.drain(..drain_end);
        }
    }

    /// Забирает warmup историю для прайминга нового tempo processor-а.
    ///
    /// Возвращает пустой Vec при несовпадении spec: праймить processor чужим
    /// PCM format нельзя.
    pub(crate) fn take_passthrough_audio_history_for_priming(
        &mut self,
        output_spec: audio_core::AudioOutputSpec,
    ) -> Vec<f32> {
        let history = std::mem::take(&mut self.passthrough_audio_history);
        let matches_spec = self.passthrough_audio_history_spec == Some(output_spec);
        self.passthrough_audio_history_spec = None;
        if matches_spec { history } else { Vec::new() }
    }

    /// Возвращает typed PCM format доступной warmup истории без чтения storage tuple снаружи.
    #[must_use]
    pub(crate) fn passthrough_audio_history_pcm_format(&self) -> Option<AudioTempoPcmFormat> {
        AudioTempoPcmFormat::from_audio_output_spec(self.passthrough_audio_history_spec?).ok()
    }

    /// Возвращает полный decoded PCM spec warmup истории для layout-safe rollback-а.
    #[must_use]
    pub(crate) const fn passthrough_audio_history_output_spec(
        &self,
    ) -> Option<audio_core::AudioOutputSpec> {
        self.passthrough_audio_history_spec
    }

    /// Сбрасывает warmup историю на seek/media boundary.
    pub(crate) fn clear_passthrough_audio_history(&mut self) {
        self.passthrough_audio_history.clear();
        self.passthrough_audio_history_spec = None;
    }

    /// Завершает stream, пишет весь DSP tail одним result и очищает processor slot.
    pub(crate) fn finish_and_clear_audio_tempo_processor(
        &mut self,
    ) -> Option<anyhow::Result<RoutedAudioTempoOutput>> {
        let mut processor = self.audio_tempo_processor.take()?;
        let stretched_output =
            match processor.finish_stream_into(&mut self.audio_tempo_output_buffer) {
                Ok(stretched_output) => stretched_output,
                Err(error) => return Some(Err(error)),
            };
        let report = stretched_output.report().clone();
        let write_status = route_audio_output_samples(
            &mut self.audio_output,
            stretched_output.interleaved_samples(),
            audio_core::AudioOutputWriteIntent::TempoProcessed,
        );

        Some(Ok(RoutedAudioTempoOutput {
            report,
            write_status,
        }))
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

    /// Ставит output на паузу и сразу переводит frozen timing в media coordinate.
    pub(crate) fn pause_audio_output_and_capture_clock(
        &mut self,
    ) -> Option<anyhow::Result<CapturedAudioClockMapping>> {
        let output = self.audio_output.as_mut()?;
        let pause_result = output.pause_and_freeze_clock();
        Some(pause_result.map(|output_timing| {
            self.audio_output_play_requested = false;
            self.captured_audio_clock_mapping_from_timing(output_timing)
        }))
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

        let ring_buffer_level_ms = output.buffer_level_ms();
        let submitted_tail_ms = output
            .clock()
            .output_timing()
            .submitted_output_tail()
            .as_secs_f64()
            * 1_000.0;
        let buffer_level_ms = if ring_buffer_level_ms.is_finite() {
            ring_buffer_level_ms.max(submitted_tail_ms)
        } else {
            submitted_tail_ms
        };
        if buffer_level_ms > 0.0 {
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
        let output_clock_position = clock.output_timing().audible_output_position();
        let playback_rate = self.audio_clock_media_mapping.open_playback_rate();
        self.audio_clock = Some(clock);
        self.audio_clock_media_mapping.reset_anchor(
            output_clock_position,
            self.media_clock_base,
            playback_rate,
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

        let playback_rate = self.audio_clock_media_mapping.open_playback_rate();
        clock.reset();
        self.audio_clock_media_mapping.reset_anchor(
            Duration::ZERO,
            self.media_clock_base,
            playback_rate,
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

    /// Возвращает согласованные audible/submitted границы neutral output clock-а.
    #[must_use]
    pub(crate) fn audio_output_clock_timing(&self) -> audio_core::AudioOutputClockTiming {
        self.audio_clock
            .as_ref()
            .map(|clock| clock.output_timing())
            .unwrap_or_else(|| {
                audio_core::AudioOutputClockTiming::new(Duration::ZERO, Duration::ZERO)
            })
    }

    /// Захватывает output timing ровно один раз и сразу применяет owned mapping.
    #[must_use]
    pub(crate) fn capture_audio_clock_mapping(&self) -> Option<CapturedAudioClockMapping> {
        let output_timing = self.audio_clock.as_ref()?.output_timing();
        Some(self.captured_audio_clock_mapping_from_timing(output_timing))
    }

    /// Захватывает frozen output coordinate, сохраняя уже опубликованную media position.
    #[must_use]
    pub(crate) fn capture_paused_audio_clock_mapping(
        &self,
        paused_media_position: Duration,
    ) -> Option<CapturedAudioClockMapping> {
        let output_timing = self.audio_clock.as_ref()?.output_timing();
        Some(CapturedAudioClockMapping::new(
            output_timing,
            paused_media_position,
        ))
    }

    /// Преобразует уже captured neutral timing без повторного clock read-а.
    #[must_use]
    fn captured_audio_clock_mapping_from_timing(
        &self,
        output_timing: audio_core::AudioOutputClockTiming,
    ) -> CapturedAudioClockMapping {
        let media_position = self
            .audio_clock_media_mapping
            .media_position_at_output_clock(output_timing.audible_output_position());
        CapturedAudioClockMapping::new(output_timing, media_position)
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
        let playback_rate = self.audio_clock_media_mapping.open_playback_rate();
        self.reanchor_audio_clock_media_mapping(position, playback_rate);
    }

    /// Перепривязывает media position к текущему output clock и playback rate.
    pub(crate) fn reanchor_audio_clock_media_mapping(
        &mut self,
        media_position: Duration,
        playback_rate: PlaybackRate,
    ) {
        self.media_clock_base = media_position;
        let output_clock_position = self.audio_output_clock_timing().audible_output_position();
        self.audio_clock_media_mapping.reset_anchor(
            output_clock_position,
            media_position,
            playback_rate,
        );
    }

    /// Перепривязывает mapping при смене playback rate, учитывая записанный output tail.
    ///
    /// Уже записанный в ring buffer output (до `audio_buffer_level`) растянут
    /// под прежний rate и доиграет с прежним темпом. Anchor фиксирует конец
    /// этого хвоста по старому mapping, чтобы новый rate применялся только к
    /// output-у, который действительно будет записан после смены.
    pub(crate) fn reanchor_audio_clock_media_mapping_for_rate_change(
        &mut self,
        media_position: Duration,
        playback_rate: PlaybackRate,
    ) {
        let captured_clock =
            CapturedAudioClockMapping::new(self.audio_output_clock_timing(), media_position);
        self.reanchor_audio_clock_media_mapping_for_captured_rate_change(
            captured_clock,
            playback_rate,
        );
    }

    /// Re-anchor по snapshot-у, который caller уже захватил для rate transaction.
    pub(crate) fn reanchor_audio_clock_media_mapping_for_captured_rate_change(
        &mut self,
        captured_clock: CapturedAudioClockMapping,
        playback_rate: PlaybackRate,
    ) {
        self.reanchor_audio_clock_media_mapping_for_rate_change_with_planned_spans(
            captured_clock,
            playback_rate,
            &[],
        );
    }

    /// Перепривязывает mapping и сохраняет pending output старых DSP segment-ов.
    pub(crate) fn reanchor_audio_clock_media_mapping_for_tempo_rate_change(
        &mut self,
        captured_clock: CapturedAudioClockMapping,
        playback_rate: PlaybackRate,
        tempo_report: &AudioTempoProcessReport,
    ) -> anyhow::Result<()> {
        let mut planned_spans = Vec::new();
        for pending_span in tempo_report
            .output_progress_mapping()
            .pending_output_segments()
        {
            let pending_rate = PlaybackRate::new(pending_span.segment().ratio().as_f64() as f32)
                .map_err(|error| {
                    anyhow::anyhow!(
                        "tempo report содержит playback rate вне player boundary: {error}"
                    )
                })?;
            planned_spans.push(PlannedAudioOutputSpan::new(
                pending_span.stretched_output().duration(),
                pending_rate,
            ));
        }

        self.reanchor_audio_clock_media_mapping_for_rate_change_with_planned_spans(
            captured_clock,
            playback_rate,
            &planned_spans,
        );
        Ok(())
    }

    /// Общая реализация re-anchor-а с уже типизированными future output spans.
    fn reanchor_audio_clock_media_mapping_for_rate_change_with_planned_spans(
        &mut self,
        captured_clock: CapturedAudioClockMapping,
        playback_rate: PlaybackRate,
        planned_spans: &[PlannedAudioOutputSpan],
    ) {
        let output_timing = captured_clock.output_timing();
        let output_clock_now = output_timing.audible_output_position();
        let media_position = captured_clock.media_position();
        self.media_clock_base = media_position;
        self.audio_clock_media_mapping
            .reanchor_for_rate_change_with_planned_spans(
                output_clock_now,
                media_position,
                output_timing.submitted_output_end_position(),
                planned_spans,
                playback_rate,
            );
    }

    /// Возвращает absolute media position, mapped from output-clock progress.
    #[must_use]
    pub(crate) fn media_position_from_audio_clock(&self) -> Duration {
        let audible_output_position = self.audio_output_clock_timing().audible_output_position();
        self.audio_clock_media_mapping
            .media_position_at_output_clock(audible_output_position)
    }

    /// Проецирует wall/output delay в media-time через кусочный tempo mapping.
    #[must_use]
    pub(crate) fn media_position_after_audio_output_delay(
        &self,
        output_delay: Duration,
    ) -> Duration {
        let audible_output_position = self.audio_output_clock_timing().audible_output_position();
        self.audio_clock_media_mapping
            .media_position_after_output_delay(audible_output_position, output_delay)
    }

    /// Возвращает wall/output delay до абсолютного media deadline.
    #[must_use]
    pub(crate) fn audio_output_delay_until_media_deadline(
        &self,
        media_deadline: Duration,
    ) -> Duration {
        let audible_output_position = self.audio_output_clock_timing().audible_output_position();
        self.audio_clock_media_mapping
            .output_delay_until_media_deadline(audible_output_position, media_deadline)
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

    /// Проецирует wall delay через активный no-audio anchor без повторного округления.
    #[must_use]
    pub(crate) fn monotonic_media_position_after_wall_delay(
        &self,
        now: Instant,
        wall_delay: Duration,
    ) -> Option<Duration> {
        self.monotonic_media_clock_anchor
            .map(|anchor| anchor.position_after_wall_delay(now, wall_delay))
    }

    /// Инвертирует media deadline через активный no-audio anchor.
    #[must_use]
    pub(crate) fn monotonic_wall_delay_until_media_deadline(
        &self,
        now: Instant,
        media_deadline: Duration,
    ) -> Option<Duration> {
        self.monotonic_media_clock_anchor
            .map(|anchor| anchor.wall_delay_until_media_deadline(now, media_deadline))
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

/// Маршрутизирует borrowed PCM, пока reusable buffer остаётся заимствованным.
fn route_audio_output_samples(
    audio_output: &mut Option<Box<dyn PlayerAudioOutput>>,
    samples: &[f32],
    intent: audio_core::AudioOutputWriteIntent,
) -> AudioOutputRoutingStatus {
    let Some(output) = audio_output.as_mut() else {
        return AudioOutputRoutingStatus::AudioOutputAbsent;
    };

    match output.write_samples(samples, intent) {
        Ok(report) => AudioOutputRoutingStatus::Written(report),
        Err(error) => AudioOutputRoutingStatus::WriteFailed(error),
    }
}

/// Бюджет warmup истории passthrough PCM в миллисекундах.
///
/// Должен с запасом покрывать startup latency tempo backend-а (у текущего
/// Signalsmith preset_default это ~120-150 ms; бюджет держит headroom и для
/// более латентных бэкендов), оставаясь bounded по памяти
/// (600 ms 48k stereo f32 ≈ 230 KB).
const PASSTHROUGH_AUDIO_HISTORY_MAX_MS: u64 = 600;

/// Считает bounded сэмпловый бюджет warmup истории для decoded PCM spec.
fn passthrough_audio_history_max_samples(sample_rate: u32, channels: u32) -> usize {
    let frames = (u64::from(sample_rate) * PASSTHROUGH_AUDIO_HISTORY_MAX_MS).div_ceil(1000);
    usize::try_from(frames * u64::from(channels)).unwrap_or(usize::MAX)
}
