use super::*;

impl PlaybackPipeline {
    /// Возвращает выбранный video track id без раскрытия storage поля.
    #[must_use]
    pub(crate) fn selected_video_track_id(&self) -> Option<TrackId> {
        self.video_track_id
    }

    /// Возвращает выбранный audio track id без раскрытия storage поля.
    #[must_use]
    pub(crate) fn selected_audio_track_id(&self) -> Option<TrackId> {
        self.audio_track_id
    }

    /// Проверяет, выбран ли video track в текущем pipeline.
    #[must_use]
    pub(crate) fn has_selected_video_track(&self) -> bool {
        self.video_track_id.is_some()
    }

    /// Проверяет, выбран ли audio track в текущем pipeline.
    #[must_use]
    pub(crate) fn has_selected_audio_track(&self) -> bool {
        self.audio_track_id.is_some()
    }

    /// Классифицирует audio runtime slots для session gate-ов без раскрытия storage полей.
    #[must_use]
    pub(crate) fn audio_seek_runtime_state(&self) -> AudioSeekRuntimeState {
        audio_seek_runtime_state_from_slots(
            self.audio_track_id.is_some(),
            self.audio_decoder.is_some(),
            self.audio_output.is_some(),
        )
    }

    /// Проверяет, относится ли video packet к активному video track.
    #[must_use]
    pub(crate) fn video_packet_belongs_to_selected_track(&self, track_id: TrackId) -> bool {
        self.video_track_id == Some(track_id)
    }

    /// Выбирает video track вместе с уже принятым session policy decode requirement.
    #[cfg(test)]
    pub(crate) fn select_video_track(
        &mut self,
        track_id: TrackId,
        requirement: VideoDecodeRequirement,
    ) {
        let frame_contract = fallback_frame_contract_for_unprobed_requirement(&requirement);
        self.select_video_track_with_frame_contract(track_id, requirement, frame_contract);
    }

    /// Выбирает video track вместе с frame contract из capability output.
    pub(crate) fn select_video_track_with_frame_contract(
        &mut self,
        track_id: TrackId,
        requirement: VideoDecodeRequirement,
        frame_contract: VideoFrameContract,
    ) {
        self.video_track_id = Some(track_id);
        self.active_video_requirement = Some(requirement);
        self.active_video_frame_contract = Some(frame_contract);
    }

    /// Выбирает audio track id без side effects для decoder/output ownership.
    pub(crate) fn select_audio_track(&mut self, track_id: TrackId) {
        self.audio_track_id = Some(track_id);
    }

    /// Очищает только выбранный audio track и связанный deferred decoder plan.
    pub(crate) fn clear_selected_audio_track(&mut self) {
        self.audio_track_id = None;
        self.clear_deferred_audio_decoder_config();
    }

    /// Очищает выбранные tracks и requirement, не трогая queues/decoder/source slots.
    pub(crate) fn clear_selected_tracks(&mut self) {
        self.audio_track_id = None;
        self.video_track_id = None;
        self.active_video_requirement = None;
        self.active_video_frame_contract = None;
    }

    /// Возвращает active video requirement без передачи владения наружу pipeline.
    #[must_use]
    pub(crate) fn active_video_requirement(&self) -> Option<&VideoDecodeRequirement> {
        self.active_video_requirement.as_ref()
    }

    /// Возвращает expected runtime frame contract active video stream-а.
    #[must_use]
    pub(crate) fn active_video_frame_contract(&self) -> Option<VideoFrameContract> {
        self.active_video_frame_contract
    }

    /// Сохраняет requirement и frame contract, которые session уже провалидировала.
    pub(crate) fn set_active_video_selection(
        &mut self,
        requirement: VideoDecodeRequirement,
        frame_contract: VideoFrameContract,
    ) {
        self.active_video_requirement = Some(requirement);
        self.active_video_frame_contract = Some(frame_contract);
    }

    /// Legacy/test helper: обновляет requirement с explicit fallback contract.
    #[cfg(test)]
    pub(crate) fn set_active_video_requirement(&mut self, requirement: VideoDecodeRequirement) {
        let frame_contract = fallback_frame_contract_for_unprobed_requirement(&requirement);
        self.set_active_video_selection(requirement, frame_contract);
    }

    /// Возвращает текущее поколение packets без раскрытия storage поля.
    #[must_use]
    pub(crate) const fn seek_generation(&self) -> u64 {
        self.seek_generation
    }

    /// Начинает новое поколение packets для seek transaction.
    ///
    /// Saturating increment оставляет поведение прежним: после переполнения
    /// generation фиксируется на `u64::MAX`, а не делает wrap в старые packets.
    pub(crate) fn begin_seek_generation(&mut self) -> u64 {
        self.seek_generation = self.seek_generation.saturating_add(1);
        self.seek_generation
    }

    /// Проверяет, относится ли packet к актуальному поколению после последнего seek.
    #[must_use]
    pub(crate) const fn packet_generation_is_current(&self, generation: u64) -> bool {
        generation == self.seek_generation
    }

    /// Возвращает текущую оценку длительности video frame без раскрытия estimator state.
    #[must_use]
    pub(crate) const fn video_frame_duration_estimate(&self) -> Duration {
        self.video_frame_duration_estimate
    }

    /// Обновляет estimator по очередному decoded PTS, сохраняя прежнюю smoothing formula.
    pub(crate) fn observe_decoded_video_frame_pts(&mut self, pts: Duration) {
        if let Some(previous_pts) = self.last_decoded_video_pts {
            let observed_duration = pts.saturating_sub(previous_pts);
            if (MIN_OBSERVED_VIDEO_FRAME_DURATION..=MAX_OBSERVED_VIDEO_FRAME_DURATION)
                .contains(&observed_duration)
            {
                let old_micros = self.video_frame_duration_estimate.as_micros() as u64;
                let new_micros = observed_duration.as_micros() as u64;
                let smoothed_micros = (old_micros.saturating_mul(7) + new_micros) / 8;
                self.video_frame_duration_estimate = Duration::from_micros(smoothed_micros.max(1));
            }
        }

        self.last_decoded_video_pts = Some(pts);
    }

    /// Возвращает estimator к bootstrap duration и забывает предыдущий decoded PTS.
    pub(crate) fn reset_video_frame_timing_estimator(&mut self) {
        self.video_frame_duration_estimate = DEFAULT_VIDEO_FRAME_DURATION;
        self.last_decoded_video_pts = None;
    }

    /// Устанавливает generation для edge-case тестов saturation без обхода boundary чтения.
    #[cfg(test)]
    pub(crate) fn set_seek_generation_for_tests(&mut self, generation: u64) {
        self.seek_generation = generation;
    }

    /// Очищает pending audio/video packets, которые относятся к старой seek generation.
    pub(crate) fn clear_pending_packets_for_seek(&mut self) {
        self.clear_pending_audio_packets();
        self.clear_pending_video_packets();
    }

    /// Сбрасывает decoder-side состояние, которое становится невалидным после seek.
    pub(crate) fn reset_decoder_state_for_seek(&mut self, has_video: bool) {
        if has_video {
            self.require_video_decoder_keyframe();
        } else {
            self.mark_video_decoder_bootstrapped();
        }
        self.reset_video_decode_in_flight();
        self.last_decoded_video_pts = None;
    }

    /// Переставляет media clocks на целевую позицию seek.
    pub(crate) fn reset_clocks_for_seek(&mut self, target: Duration) {
        self.reanchor_media_clock_for_seek(target, Instant::now());
    }

    /// Перепривязывает seek clock base без доступа к внутренним clock полям.
    pub(crate) fn reanchor_media_clock_for_seek(
        &mut self,
        position: Duration,
        observed_at: Instant,
    ) {
        self.set_media_clock_base(position);
        self.clear_monotonic_media_clock();
        self.reset_audio_clock_sample(Duration::ZERO, observed_at);
    }

    /// Очищает очередь будущих video frames и возвращает texture handles для release.
    #[must_use]
    pub(crate) fn clear_video_queues(&mut self) -> Vec<video_core::FrameResourceHandle> {
        self.video_frame_queue
            .drain(..)
            .map(|frame| frame.resource_handle)
            .collect()
    }

    /// Возвращает текущий present frame без передачи владения наружу pipeline.
    #[must_use]
    pub(crate) fn present_video_frame(&self) -> Option<&video_core::DecodedFrame> {
        self.present_video_frame.as_ref()
    }

    /// Возвращает PTS текущего present frame-а для diagnostics и seek gates.
    #[must_use]
    pub(crate) fn present_video_frame_pts(&self) -> Option<Duration> {
        self.present_video_frame().map(|frame| frame.pts)
    }

    /// Проверяет, что текущий present frame покрывает целевую media-позицию.
    #[must_use]
    pub(crate) fn present_video_frame_covers(&self, target: Duration) -> bool {
        self.present_video_frame()
            .is_some_and(|frame| frame.pts >= target)
    }

    /// Проверяет, что текущий present frame ровно совпадает с media-позицией.
    #[must_use]
    pub(crate) fn present_video_frame_matches(&self, position: Duration) -> bool {
        self.present_video_frame()
            .is_some_and(|frame| frame.pts == position)
    }

    /// Проверяет наличие текущего present frame-а без раскрытия внутреннего `Option`.
    #[must_use]
    pub(crate) fn has_present_video_frame(&self) -> bool {
        self.present_video_frame.is_some()
    }

    /// Делает decoded frame текущим кадром presentation.
    pub(crate) fn set_present_video_frame(&mut self, frame: video_core::DecodedFrame) {
        self.present_video_frame = Some(frame);
    }

    /// Забирает текущий present frame, чтобы вызывающий слой мог освободить texture.
    pub(crate) fn take_present_video_frame(&mut self) -> Option<video_core::DecodedFrame> {
        self.present_video_frame.take()
    }

    /// Заменяет текущий present frame и возвращает старый frame для явного release.
    pub(crate) fn replace_present_video_frame(
        &mut self,
        frame: video_core::DecodedFrame,
    ) -> Option<video_core::DecodedFrame> {
        let old_frame = self.take_present_video_frame();
        self.set_present_video_frame(frame);
        old_frame
    }

    /// Проверяет наличие EOF fallback frame-а для final seek near EOF.
    #[must_use]
    pub(crate) fn has_seek_preroll_fallback_video_frame(&self) -> bool {
        self.seek_preroll_fallback_video_frame.is_some()
    }

    /// Забирает EOF fallback frame, когда scheduler решил показать его после EOF.
    pub(crate) fn take_seek_preroll_fallback_video_frame(
        &mut self,
    ) -> Option<video_core::DecodedFrame> {
        self.seek_preroll_fallback_video_frame.take()
    }

    /// Заменяет EOF fallback frame и возвращает прежний frame для явного release.
    pub(crate) fn replace_seek_preroll_fallback_video_frame(
        &mut self,
        frame: video_core::DecodedFrame,
    ) -> Option<video_core::DecodedFrame> {
        self.seek_preroll_fallback_video_frame.replace(frame)
    }

    /// Очищает EOF fallback frame и возвращает его владельцу release path-а.
    pub(crate) fn clear_seek_preroll_fallback_video_frame(
        &mut self,
    ) -> Option<video_core::DecodedFrame> {
        self.seek_preroll_fallback_video_frame.take()
    }

    /// Добавляет audio packet в pending queue текущего pipeline.
    pub(crate) fn enqueue_pending_audio_packet(&mut self, packet: PendingAudioPacket) {
        self.pending_audio_packets.push_back(packet);
    }

    /// Добавляет video packet в pending queue текущего pipeline.
    pub(crate) fn enqueue_pending_video_packet(&mut self, packet: PendingVideoPacket) {
        self.pending_video_packets.push_back(packet);
    }

    /// Забирает первый pending video packet для drop или отправки в decoder.
    pub(crate) fn pop_pending_video_packet_front(&mut self) -> Option<PendingVideoPacket> {
        self.pending_video_packets.pop_front()
    }

    /// Возвращает первый pending video packet без снятия его с очереди.
    #[must_use]
    pub(crate) fn front_pending_video_packet(&self) -> Option<&PendingVideoPacket> {
        self.pending_video_packets.front()
    }

    /// Проверяет, пуста ли очередь pending video packets.
    #[must_use]
    pub(crate) fn pending_video_packet_is_empty(&self) -> bool {
        self.pending_video_packets.is_empty()
    }

    /// Очищает очередь pending video packets через единый pipeline boundary.
    pub(crate) fn clear_pending_video_packets(&mut self) {
        self.pending_video_packets.clear();
        // Seek/media reset репозиционируют demux на keyframe сами: ожидание
        // keyframe-а после audio catch-up shed здесь больше не актуально.
        self.video_admission_waits_for_keyframe = false;
    }

    /// Сбрасывает video backlog ради audio catch-up и ждёт следующий keyframe.
    ///
    /// Декодер к этому моменту на секунды позади live-позиции: backlog всё
    /// равно был бы отброшен как late после декода, но пока он занимает
    /// очередь, demux стоит и audio-пакеты не поступают. После сброса admission
    /// пропускает только следующий keyframe, чтобы декодер остался decodable.
    pub(crate) fn shed_pending_video_backlog_for_audio_catchup(&mut self) -> usize {
        let shed_packets = self.pending_video_packets.len();
        self.pending_video_packets.clear();
        self.video_admission_waits_for_keyframe = true;
        shed_packets
    }

    /// Решает, должен ли admission отбросить video packet до keyframe-resync.
    ///
    /// `Unknown` пропускается: контейнер без keyframe-классификации иначе
    /// заморозил бы видео навсегда, а decoder восстанавливается сам.
    pub(crate) fn video_admission_should_drop_for_keyframe_resync(
        &mut self,
        keyframe: media_core::PacketKeyframe,
    ) -> bool {
        if !self.video_admission_waits_for_keyframe {
            return false;
        }

        match keyframe {
            media_core::PacketKeyframe::NotKeyframe => true,
            media_core::PacketKeyframe::Keyframe | media_core::PacketKeyframe::Unknown => {
                self.video_admission_waits_for_keyframe = false;
                false
            }
        }
    }

    /// Забирает первый pending audio packet для декодирования.
    pub(crate) fn pop_pending_audio_packet_front(&mut self) -> Option<PendingAudioPacket> {
        self.pending_audio_packets.pop_front()
    }

    /// Возвращает audio packet обратно в начало очереди после throttle.
    pub(crate) fn push_pending_audio_packet_front(&mut self, packet: PendingAudioPacket) {
        self.pending_audio_packets.push_front(packet);
    }

    /// Проверяет, пуста ли очередь pending audio packets.
    #[must_use]
    pub(crate) fn pending_audio_packet_is_empty(&self) -> bool {
        self.pending_audio_packets.is_empty()
    }

    /// Возвращает глубину pending audio queue без раскрытия поля очереди.
    #[must_use]
    pub(crate) fn pending_audio_packet_len(&self) -> usize {
        self.pending_audio_packets.len()
    }

    /// Очищает очередь pending audio packets через единый pipeline boundary.
    pub(crate) fn clear_pending_audio_packets(&mut self) {
        self.pending_audio_packets.clear();
    }

    /// Возвращает первый decoded frame из presentation queue без мутации.
    #[must_use]
    pub(crate) fn front_queued_video_frame(&self) -> Option<&video_core::DecodedFrame> {
        self.video_frame_queue.front()
    }

    /// Даёт read-only проход по presentation queue без доступа к самой структуре очереди.
    pub(crate) fn queued_video_frames(&self) -> impl Iterator<Item = &video_core::DecodedFrame> {
        self.video_frame_queue.iter()
    }

    /// Возвращает первый и следующий за ним decoded frames без раскрытия очереди.
    #[must_use]
    pub(crate) fn front_and_next_queued_video_frames(
        &self,
    ) -> Option<(&video_core::DecodedFrame, &video_core::DecodedFrame)> {
        let front_frame = self.video_frame_queue.front()?;
        let next_frame = self.video_frame_queue.get(1)?;

        Some((front_frame, next_frame))
    }

    /// Забирает первый decoded frame из presentation queue.
    pub(crate) fn pop_queued_video_frame_front(&mut self) -> Option<video_core::DecodedFrame> {
        self.video_frame_queue.pop_front()
    }

    /// Добавляет decoded frame в конец presentation queue.
    pub(crate) fn enqueue_queued_video_frame(&mut self, frame: video_core::DecodedFrame) {
        self.video_frame_queue.push_back(frame);
    }

    /// Проверяет, пуста ли presentation queue.
    #[must_use]
    pub(crate) fn video_present_queue_is_empty(&self) -> bool {
        self.video_frame_queue.is_empty()
    }

    /// Проверяет, есть ли queued frame текущего seek generation-а для final target.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn queued_video_frame_covers_target_for_generation(
        &self,
        target: Duration,
        generation: u64,
    ) -> bool {
        self.video_frame_queue
            .iter()
            .any(|frame| frame.generation == generation && frame.pts >= target)
    }

    /// Возвращает глубину presentation queue без раскрытия поля очереди.
    #[must_use]
    pub(crate) fn video_present_queue_len(&self) -> usize {
        self.video_frame_queue.len()
    }

    /// Возвращает глубину pending video queue без раскрытия поля очереди.
    #[must_use]
    pub(crate) fn pending_video_packet_len(&self) -> usize {
        self.pending_video_packets.len()
    }

    /// Проверяет, ждёт ли video decoder первый keyframe после bootstrap/flush.
    #[must_use]
    pub(crate) const fn video_decoder_needs_keyframe(&self) -> bool {
        self.video_decoder_needs_keyframe
    }

    /// Отмечает, что decoder получил keyframe и может принимать inter-frames.
    pub(crate) fn mark_video_decoder_bootstrapped(&mut self) {
        self.video_decoder_needs_keyframe = false;
    }

    /// Требует новый keyframe перед следующей отправкой packets в decoder.
    pub(crate) fn require_video_decoder_keyframe(&mut self) {
        self.video_decoder_needs_keyframe = true;
    }

    /// Переводит renderer resource ids в новое поколение после полной смены media.
    pub(crate) fn advance_render_generation(&mut self) {
        self.rendered_video_texture_release_providers.clear();
        self.render_generation = self.render_generation.wrapping_add(1);
    }

    /// Возвращает текущее поколение render resources без доступа к полю accounting-а.
    #[must_use]
    pub(crate) const fn render_generation(&self) -> u64 {
        self.render_generation
    }

    /// Сбрасывает все media-specific поля после того, как session освободила video frames.
    pub(crate) fn reset_media_slots(&mut self) {
        self.clear_media_source_slots();
        self.clear_audio_decoder();
        self.clear_deferred_audio_decoder_config();
        self.clear_audio_output();
        self.next_audio_tempo_segment_id = 1;
        self.clear_selected_tracks();
        self.clear_pending_audio_packets();
        self.clear_pending_video_packets();
        self.rendered_video_texture_release_providers.clear();
        self.require_video_decoder_keyframe();
        self.reset_video_decode_in_flight();
        debug_assert!(
            self.seek_preroll_fallback_video_frame.is_none(),
            "reset_media_slots вызывается только после release всех video frames"
        );
        self.seek_preroll_fallback_video_frame = None;
        self.reanchor_audio_clock_media_mapping(Duration::ZERO, PlaybackRate::NORMAL);
        self.clear_monotonic_media_clock();
        self.seek_generation = 0;
        self.mark_audio_buffer_clear_ack(0);
        self.reset_video_frame_timing_estimator();
        self.reset_audio_clock_sample(Duration::ZERO, Instant::now());
    }

    /// Очищает только source identity и demux handle без изменения остальных lifecycle slots.
    fn clear_media_source_slots(&mut self) {
        self.demuxer = None;
        self.file_path = None;
        self.tracks.clear();
        self.source_label = None;
    }

    /// Подключает уже открытый demuxer и source identity к текущему pipeline.
    pub(crate) fn install_opened_media(
        &mut self,
        demuxer: Box<dyn Demuxer + Send>,
        file_path: Option<PathBuf>,
        source_label: Option<String>,
        tracks: Vec<TrackInfo>,
    ) {
        self.demuxer = Some(demuxer);
        self.file_path = file_path;
        self.source_label = source_label;
        self.tracks = tracks;
    }

    /// Применяет новый track list после demux lifecycle reset.
    ///
    /// Demuxer остаётся тем же владельцем source-а, но decoder-dependent state
    /// должен быть пересоздан, потому что старые configs могли ссылаться на уже
    /// неактуальные track ids, codec params или audio spec.
    pub(crate) fn apply_demux_track_list_update(&mut self, tracks: Vec<TrackInfo>) {
        let has_video_track = tracks
            .iter()
            .any(|track| track.kind == media_core::TrackKind::Video);

        self.tracks = tracks;
        self.clear_audio_decoder();
        self.clear_deferred_audio_decoder_config();
        self.clear_audio_output();
        self.clear_selected_tracks();
        self.clear_pending_audio_packets();
        self.clear_pending_video_packets();
        self.mark_audio_buffer_clear_ack(self.seek_generation);
        self.reset_decoder_state_for_seek(has_video_track);
    }

    /// Проверяет, установлен ли demuxer текущего media source.
    #[must_use]
    pub(crate) fn has_demuxer(&self) -> bool {
        self.demuxer.is_some()
    }

    /// Возвращает путь локального source без передачи владения path storage.
    #[must_use]
    pub(crate) fn source_file_path(&self) -> Option<&Path> {
        self.file_path.as_deref()
    }

    /// Возвращает streaming/source label без раскрытия внутреннего `Option<String>`.
    #[must_use]
    pub(crate) fn source_label(&self) -> Option<&str> {
        self.source_label.as_deref()
    }

    /// Возвращает immutable tracks snapshot текущего media.
    #[must_use]
    pub(crate) fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    /// Возвращает количество tracks, когда вызывающему коду не нужны сами metadata.
    #[must_use]
    pub(crate) fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Читает следующий packet через demux boundary, сохраняя absent-demuxer как no-op.
    #[cfg(test)]
    pub(crate) fn demux_next_packet(
        &mut self,
    ) -> Option<anyhow::Result<Option<media_core::Packet>>> {
        self.demuxer.as_mut().map(|demuxer| demuxer.next_packet())
    }

    /// Читает следующий demux event, сохраняя absent-demuxer как no-op.
    pub(crate) fn demux_next_event(&mut self) -> Option<anyhow::Result<DemuxReadEvent>> {
        self.demuxer.as_mut().map(|demuxer| demuxer.next_event())
    }

    /// Выполняет seek текущего demuxer-а, не раскрывая место хранения demux handle.
    pub(crate) fn seek_demuxer(
        &mut self,
        request: DemuxSeekRequest,
    ) -> Option<anyhow::Result<DemuxSeekResult>> {
        self.demuxer
            .as_mut()
            .map(|demuxer| demuxer.seek_with_request(request))
    }
}
