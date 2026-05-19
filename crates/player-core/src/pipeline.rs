use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::VideoDecodeRequirement;
use media_core::{DemuxSeekRequest, DemuxSeekResult, Demuxer, TrackId, TrackInfo};

use crate::{
    DecodeSendError, DecodeThreadError, DecoderControlChannelPressureSnapshot,
    DecoderResourceSnapshot, PlayerDecodePacket, PlayerVideoDecoderThreadHandle,
    WgpuRenderTextureProviderHandle,
};
#[cfg(test)]
use video_core::VideoDecoderThreadHandle;

/// Bootstrap-оценка длительности frame до первых PTS observations; не worker cadence.
pub(crate) const DEFAULT_VIDEO_FRAME_DURATION: Duration = Duration::from_micros(16_667);

/// Минимальная разумная длительность кадра для оценки FPS.
pub(crate) const MIN_OBSERVED_VIDEO_FRAME_DURATION: Duration = Duration::from_millis(5);

/// Максимальная разумная длительность кадра для оценки FPS.
pub(crate) const MAX_OBSERVED_VIDEO_FRAME_DURATION: Duration = Duration::from_millis(100);

/// Результат снятия render lease-а из accounting map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderLeaseReleaseEffect {
    /// Drop-ack пришёл для lease-а, которого pipeline уже не учитывает.
    UnknownLease,

    /// Lease count уменьшен, но другие clone-ы всё ещё держат texture handle.
    LeaseStillActive,

    /// Последний lease снят, deferred texture release для handle не был запрошен.
    ReleasedWithoutDeferredTexture,

    /// Последний lease снят, и ранее отложенный texture release можно выполнить.
    DeferredTextureReady,
}

/// Решение pipeline accounting при запросе release texture handle-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoTextureReleaseEffect {
    /// Texture handle удерживается renderer-ом и должен быть освобождён после drop-ack.
    DeferredUntilRenderLeaseDrop,

    /// Активных render leases нет, texture handle можно сразу вернуть decoder-у.
    ReleaseNow,
}

/// Anchor внутреннего monotonic media clock для media без audio clock.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MonotonicMediaClockAnchor {
    /// Media position, которая соответствовала `anchored_at`.
    media_position: Duration,

    /// Монотонный момент, от которого считается fallback media time.
    anchored_at: Instant,
}

impl MonotonicMediaClockAnchor {
    /// Создаёт anchor без привязки к video FPS или worker tick cadence.
    #[must_use]
    pub(crate) const fn new(media_position: Duration, anchored_at: Instant) -> Self {
        Self {
            media_position,
            anchored_at,
        }
    }

    /// Возвращает media position на заданный monotonic момент.
    #[must_use]
    pub(crate) fn position_at(self, now: Instant) -> Duration {
        self.media_position
            .checked_add(now.saturating_duration_since(self.anchored_at))
            .unwrap_or(Duration::MAX)
    }
}

/// Сырой audio packet, который ждёт decode из-за backpressure audio buffer.
pub(crate) struct PendingAudioPacket {
    /// Track ID нужен, чтобы не отправить packet неактивного audio track в decoder.
    pub(crate) track_id: TrackId,

    /// Presentation timestamp packet-а на абсолютной media timeline.
    pub(crate) pts: Duration,

    /// Seek generation, в котором packet был прочитан из demuxer.
    pub(crate) generation: u64,

    /// Encoded audio bytes владеют shared payload-ом без копии между demuxer и player queue.
    pub(crate) encoded_bytes: Bytes,
}

impl PendingAudioPacket {
    /// Создаёт ожидающий audio packet с явным track id и codec bytes.
    #[must_use]
    pub(crate) fn new(
        track_id: TrackId,
        pts: Duration,
        generation: u64,
        encoded_bytes: Bytes,
    ) -> Self {
        Self {
            track_id,
            pts,
            generation,
            encoded_bytes,
        }
    }
}

/// Сырой video packet, который ждёт отправки в decode thread.
pub(crate) struct PendingVideoPacket {
    /// Track ID нужен для фильтрации выбранного video track.
    pub(crate) track_id: TrackId,

    /// Presentation timestamp определяет A/V sync и decode-ahead лимит.
    pub(crate) pts: Duration,

    /// Seek generation, в котором packet был прочитан из demuxer.
    pub(crate) generation: u64,

    /// Encoded video bytes владеют shared payload-ом без копии до decoder thread.
    pub(crate) encoded_bytes: Bytes,

    /// Keyframe flag пробрасывается в hardware decoder.
    pub(crate) keyframe: bool,
}

impl PendingVideoPacket {
    /// Создаёт ожидающий video packet без неименованных tuple-полей.
    #[must_use]
    pub(crate) fn new(
        track_id: TrackId,
        pts: Duration,
        generation: u64,
        encoded_bytes: Bytes,
        keyframe: bool,
    ) -> Self {
        Self {
            track_id,
            pts,
            generation,
            encoded_bytes,
            keyframe,
        }
    }
}

/// Внутреннее владение media pipeline для текущей player session.
///
/// Поля остаются видимыми только внутри `player-core`, пока tick/scheduler
/// живут отдельным модулем. Наружный API работает через методы `PlayerSession`.
pub(crate) struct PlaybackPipeline {
    /// Demuxer текущего media source через нейтральный media-core contract.
    demuxer: Option<Box<dyn media_core::Demuxer + Send>>,

    /// Локальный путь, если media был открыт из файловой системы.
    file_path: Option<PathBuf>,

    /// Tracks текущего media без доступа UI к demuxer handle.
    tracks: Vec<TrackInfo>,

    /// User-facing label для streaming source без локального path.
    source_label: Option<String>,

    /// Codec-neutral audio decoder для выбранного audio трека.
    pub(crate) audio_decoder: Option<audio::AudioDecoderHandle>,

    /// Audio output: CPAL stream и ring buffer.
    pub(crate) audio_output: Option<audio::AudioOutput>,

    /// Track ID выбранного audio трека.
    pub(crate) audio_track_id: Option<TrackId>,

    /// Очередь сырых audio packets для throttle.
    pub(crate) pending_audio_packets: VecDeque<PendingAudioPacket>,

    /// Video decoder thread: backend decode в отдельном потоке за узким session contract.
    pub(crate) video_decoder_thread: Option<Box<PlayerVideoDecoderThreadHandle>>,

    /// Требует ли decoder следующий video packet быть keyframe-ом.
    ///
    /// Stateless hardware decode после flush теряет reference frames. Если сразу
    /// отправить inter-frame, decoder может показать зелёные артефакты до
    /// следующего keyframe. Флаг держит этот codec contract рядом с очередью
    /// packets, а не в UI/render слое.
    pub(crate) video_decoder_needs_keyframe: bool,

    /// Сколько video packets уже ушло в decoder thread и ещё не получило packet ack.
    ///
    /// `VideoDecodeThread::packet_queue_depth()` не видит packet, который decoder
    /// уже забрал из channel и прямо сейчас обрабатывает. Decoder ack приходит
    /// по отдельному channel независимо от того, дал packet output frame или нет.
    pub(crate) video_decode_in_flight_packets: usize,

    /// Очередь декодированных видеокадров перед presentation.
    video_frame_queue: VecDeque<video_core::DecodedFrame>,

    /// Последний свежедекодированный frame до final seek target для EOF fallback.
    ///
    /// При seek в самый конец файла requested target может оказаться позже
    /// последнего реального video PTS. Такой frame нельзя показывать сразу как
    /// точный target, но его нужно сохранить до EOF, чтобы не зависнуть в seek.
    seek_preroll_fallback_video_frame: Option<video_core::DecodedFrame>,

    /// Текущая оценка длительности одного video frame.
    pub(crate) video_frame_duration_estimate: Duration,

    /// PTS последнего decoded frame для обновления оценки frame duration.
    pub(crate) last_decoded_video_pts: Option<Duration>,

    /// Текущий кадр для отображения, выбранный scheduler.
    present_video_frame: Option<video_core::DecodedFrame>,

    /// Поколение render resources текущего media pipeline.
    pub(crate) render_generation: u64,

    /// Texture handles, которые сейчас удерживает render/UI thread.
    pub(crate) leased_video_textures: HashMap<(u64, u64), usize>,

    /// Texture handles, release которых отложен до drop-ack от render/UI thread.
    pub(crate) deferred_video_texture_releases: HashSet<(u64, u64)>,

    /// Track ID выбранного video трека.
    pub(crate) video_track_id: Option<TrackId>,

    /// Очередь сырых video packets для decode.
    pub(crate) pending_video_packets: VecDeque<PendingVideoPacket>,

    /// Audio clock для A/V sync.
    pub(crate) audio_clock: Option<Arc<audio::clock::AudioClock>>,

    /// Абсолютная media-позиция, соответствующая нулю текущего audio clock.
    pub(crate) media_clock_base: Duration,

    /// Внутренний monotonic clock для playback без доступного audio clock.
    pub(crate) monotonic_media_clock_anchor: Option<MonotonicMediaClockAnchor>,

    /// Поколение packets после последнего seek transaction.
    pub(crate) seek_generation: u64,

    /// Последнее поколение, для которого audio output подтвердил очистку buffer.
    pub(crate) audio_buffer_clear_generation: u64,

    /// Последнее значение audio clock для обнаружения stalled audio.
    pub(crate) last_audio_clock: Duration,

    /// Момент последнего изменения audio clock.
    pub(crate) last_audio_clock_change_at: Instant,

    /// Индикатор текущего видео backend для UI и диагностики.
    pub(crate) video_backend: &'static str,

    /// Требование активного video track, уточнённое container metadata или bitstream probe.
    pub(crate) active_video_requirement: Option<VideoDecodeRequirement>,
}

impl PlaybackPipeline {
    /// Начинает новое поколение packets для seek transaction.
    ///
    /// Saturating increment оставляет поведение прежним: после переполнения
    /// generation фиксируется на `u64::MAX`, а не делает wrap в старые packets.
    pub(crate) fn begin_seek_generation(&mut self) -> u64 {
        self.seek_generation = self.seek_generation.saturating_add(1);
        self.seek_generation
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
        self.media_clock_base = target;
        self.monotonic_media_clock_anchor = None;
        self.last_audio_clock = Duration::ZERO;
        self.last_audio_clock_change_at = Instant::now();
    }

    /// Очищает очередь будущих video frames и возвращает texture handles для release.
    #[must_use]
    pub(crate) fn clear_video_queues(&mut self) -> Vec<video_core::FrameTextureHandle> {
        self.video_frame_queue
            .drain(..)
            .map(|frame| frame.texture_handle)
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
    #[must_use]
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
        self.audio_decoder = None;
        self.audio_output = None;
        self.audio_track_id = None;
        self.clear_pending_audio_packets();
        self.video_track_id = None;
        self.clear_pending_video_packets();
        self.require_video_decoder_keyframe();
        self.reset_video_decode_in_flight();
        debug_assert!(
            self.seek_preroll_fallback_video_frame.is_none(),
            "reset_media_slots вызывается только после release всех video frames"
        );
        self.seek_preroll_fallback_video_frame = None;
        self.audio_clock = None;
        self.media_clock_base = Duration::ZERO;
        self.monotonic_media_clock_anchor = None;
        self.seek_generation = 0;
        self.audio_buffer_clear_generation = 0;
        self.video_frame_duration_estimate = DEFAULT_VIDEO_FRAME_DURATION;
        self.last_decoded_video_pts = None;
        self.last_audio_clock = Duration::ZERO;
        self.last_audio_clock_change_at = Instant::now();
        self.active_video_requirement = None;
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
    pub(crate) fn demux_next_packet(
        &mut self,
    ) -> Option<anyhow::Result<Option<media_core::Packet>>> {
        self.demuxer.as_mut().map(|demuxer| demuxer.next_packet())
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

    /// Сохраняет запущенный video backend без раскрытия backend-specific init в session.
    #[cfg(test)]
    pub(crate) fn set_video_decoder_thread(
        &mut self,
        decoder_thread: impl VideoDecoderThreadHandle<
            TextureViewProvider = WgpuRenderTextureProviderHandle,
        > + 'static,
    ) {
        self.set_video_decoder_thread_handle(Box::new(decoder_thread));
    }

    /// Сохраняет decoder handle, который уже прошёл backend startup boundary.
    pub(crate) fn set_video_decoder_thread_handle(
        &mut self,
        decoder_thread: Box<PlayerVideoDecoderThreadHandle>,
    ) {
        self.video_backend = decoder_thread.backend_name();
        self.video_decoder_thread = Some(decoder_thread);
        self.reset_video_decode_in_flight();
    }

    /// Проверяет, можно ли отправлять encoded packets через decoder I/O boundary.
    ///
    /// Tick-код использует этот метод как send-side readiness и не зависит от
    /// того, каким полем pipeline владеет активным decoder backend-ом.
    #[must_use]
    pub(crate) fn can_send_video_decode_packets(&self) -> bool {
        self.video_decoder_thread.is_some()
    }

    /// Проверяет, можно ли принимать decoded frames через decoder I/O boundary.
    ///
    /// Receive-side readiness сейчас совпадает с наличием active backend-а, но
    /// call sites больше не читают внутреннее устройство decoder thread-а.
    #[must_use]
    pub(crate) fn can_receive_decoded_video_frames(&self) -> bool {
        self.video_decoder_thread.is_some()
    }

    /// Возвращает глубину send queue decoder thread-а, если backend запущен.
    #[must_use]
    pub(crate) fn video_decoder_packet_queue_depth(&self) -> Option<usize> {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.packet_queue_depth())
    }

    /// Возвращает snapshot texture pool-а, не раскрывая decoder thread наружу.
    #[must_use]
    pub(crate) fn video_decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.decoder_resource_snapshot())
    }

    /// Возвращает pressure snapshot decoder control channel-а, если backend его поддерживает.
    pub(crate) fn video_decoder_control_channel_pressure(
        &self,
    ) -> Option<DecoderControlChannelPressureSnapshot> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.decoder_control_channel_pressure())
    }

    /// Возвращает WGPU provider для renderer-side texture views активного decoder thread-а.
    #[must_use]
    pub(crate) fn video_decoder_texture_view_provider(
        &self,
    ) -> Option<WgpuRenderTextureProviderHandle> {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.texture_view_provider())
    }

    /// Немедленно отдаёт texture slot активному decoder thread-у.
    ///
    /// Метод намеренно не знает про deferred render leases: это решение остаётся
    /// в `PlayerSession`, потому что только session видит поколение renderer-а.
    pub(crate) fn release_frame_to_video_decoder(
        &self,
        texture_handle: video_core::FrameTextureHandle,
    ) -> bool {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return false;
        };

        decoder_thread.release_frame(texture_handle);
        true
    }

    /// Сбрасывает decoder thread перед seek/media reset.
    ///
    /// Отсутствующий decoder thread остаётся успешным no-op, как и прежний
    /// прямой вызов из session.
    pub(crate) fn flush_video_decoder_thread(&self) -> anyhow::Result<()> {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return Ok(());
        };

        decoder_thread.flush()
    }

    /// Забирает один decoded frame без блокировки worker-а.
    pub(crate) fn try_recv_decoded_video_frame(&self) -> Option<video_core::DecodedFrame> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.try_recv_frame())
    }

    /// Забирает один diagnostics event от decoder/backend boundary.
    pub(crate) fn try_recv_video_decoder_diagnostic_event(
        &self,
    ) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.try_recv_diagnostic_event())
    }

    /// Забирает один fatal decoder-thread error, если backend уже остановился.
    pub(crate) fn try_recv_video_decoder_error(&self) -> Option<DecodeThreadError> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.try_recv_error())
    }

    /// Забирает packet ack-и decoder thread-а без изменения player-side accounting.
    #[must_use]
    pub(crate) fn drain_completed_video_decode_packet_count(&self) -> usize {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.drain_completed_packet_count())
            .unwrap_or(0)
    }

    /// Отправляет encoded packet в активный decoder thread.
    ///
    /// `None` означает, что decoder thread отсутствует. `Some(Ok(()))`
    /// означает принятую отправку, а `Some(Err(_))` сохраняет различие между
    /// backpressure и fatal send failure.
    pub(crate) fn send_video_decode_packet(
        &self,
        packet: PlayerDecodePacket,
    ) -> Option<Result<(), DecodeSendError>> {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.send_packet(packet))
    }

    /// Сбрасывает счётчик packets, которые могли остаться внутри decoder после flush/seek.
    pub(crate) fn reset_video_decode_in_flight(&mut self) {
        self.video_decode_in_flight_packets = 0;
    }

    /// Отмечает packet, успешно переданный через worker -> decoder boundary.
    pub(crate) fn note_video_packet_sent_to_decoder(&mut self) {
        self.video_decode_in_flight_packets = self.video_decode_in_flight_packets.saturating_add(1);
    }

    /// Отмечает packets, которые decoder thread обработал без привязки к числу output frames.
    pub(crate) fn note_video_packets_completed_by_decoder(&mut self, packet_count: usize) {
        self.video_decode_in_flight_packets = self
            .video_decode_in_flight_packets
            .saturating_sub(packet_count);
    }

    /// Возвращает приблизительное число packets, которые decoder уже забрал, но ещё не ack-нул.
    #[must_use]
    pub(crate) const fn video_decode_in_flight_packets(&self) -> usize {
        self.video_decode_in_flight_packets
    }

    /// Регистрирует render lease только для актуального поколения renderer resources.
    pub(crate) fn try_register_render_lease(
        &mut self,
        render_generation: u64,
        texture_handle: video_core::FrameTextureHandle,
    ) -> bool {
        if render_generation != self.render_generation {
            return false;
        }

        let lease_key = (render_generation, texture_handle.0);
        let lease_count = self.leased_video_textures.entry(lease_key).or_insert(0);
        *lease_count = lease_count.saturating_add(1);
        true
    }

    /// Снимает один render lease и возвращает точный accounting outcome.
    pub(crate) fn release_render_lease_accounting(
        &mut self,
        render_generation: u64,
        texture_handle: video_core::FrameTextureHandle,
    ) -> RenderLeaseReleaseEffect {
        let lease_key = (render_generation, texture_handle.0);

        let Some(lease_count) = self.leased_video_textures.get_mut(&lease_key) else {
            return RenderLeaseReleaseEffect::UnknownLease;
        };

        if *lease_count > 1 {
            *lease_count -= 1;
            return RenderLeaseReleaseEffect::LeaseStillActive;
        }

        self.leased_video_textures.remove(&lease_key);
        if self.deferred_video_texture_releases.remove(&lease_key) {
            RenderLeaseReleaseEffect::DeferredTextureReady
        } else {
            RenderLeaseReleaseEffect::ReleasedWithoutDeferredTexture
        }
    }

    /// Помечает texture handle текущего поколения как deferred, если renderer держит lease.
    pub(crate) fn request_video_texture_release(
        &mut self,
        texture_handle: video_core::FrameTextureHandle,
    ) -> VideoTextureReleaseEffect {
        let lease_key = (self.render_generation, texture_handle.0);
        if self.leased_video_textures.contains_key(&lease_key) {
            self.deferred_video_texture_releases.insert(lease_key);
            return VideoTextureReleaseEffect::DeferredUntilRenderLeaseDrop;
        }

        VideoTextureReleaseEffect::ReleaseNow
    }

    /// Возвращает количество texture handles, удерживаемых render leases.
    #[must_use]
    pub(crate) fn active_render_lease_count(&self) -> usize {
        self.leased_video_textures.len()
    }

    /// Проверяет, отложен ли release конкретного texture handle текущего поколения.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_deferred_video_texture_release(
        &self,
        texture_handle: video_core::FrameTextureHandle,
    ) -> bool {
        self.deferred_video_texture_releases
            .contains(&(self.render_generation, texture_handle.0))
    }

    /// Возвращает количество отложенных texture releases.
    #[must_use]
    pub(crate) fn deferred_render_release_count(&self) -> usize {
        self.deferred_video_texture_releases.len()
    }
}

impl Default for PlaybackPipeline {
    /// Возвращает начальные значения pipeline без decoder/demuxer/audio ресурсов.
    fn default() -> Self {
        Self {
            demuxer: None,
            file_path: None,
            tracks: Vec::new(),
            source_label: None,
            audio_decoder: None,
            audio_output: None,
            audio_track_id: None,
            pending_audio_packets: VecDeque::new(),
            video_decoder_thread: None,
            video_decoder_needs_keyframe: true,
            video_decode_in_flight_packets: 0,
            video_frame_queue: VecDeque::new(),
            seek_preroll_fallback_video_frame: None,
            video_frame_duration_estimate: DEFAULT_VIDEO_FRAME_DURATION,
            last_decoded_video_pts: None,
            present_video_frame: None,
            render_generation: 0,
            leased_video_textures: HashMap::new(),
            deferred_video_texture_releases: HashSet::new(),
            video_track_id: None,
            pending_video_packets: VecDeque::new(),
            audio_clock: None,
            media_clock_base: Duration::ZERO,
            monotonic_media_clock_anchor: None,
            seek_generation: 0,
            audio_buffer_clear_generation: 0,
            last_audio_clock: Duration::ZERO,
            last_audio_clock_change_at: Instant::now(),
            video_backend: "Synthetic (test)",
            active_video_requirement: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use media_core::{MediaTime, TrackKind};

    /// Создаёт decoded frame без реальных GPU resources для проверки pipeline storage.
    fn decoded_frame_for_tests(pts: Duration, texture_handle: u64) -> video_core::DecodedFrame {
        video_core::DecodedFrame {
            pts,
            format: video_core::DecodedPixelFormat::Nv12,
            bit_depth: codec_core::BitDepth::Eight,
            chroma: codec_core::ChromaSubsampling::Yuv420,
            memory_path: video_core::FrameMemoryPath::DmaBufZeroCopy,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: codec_core::VideoColorMetadata::sdr_bt709_limited(),
            texture_handle: video_core::FrameTextureHandle(texture_handle),
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

    #[test]
    fn queued_video_frame_methods_preserve_fifo_order_and_len() {
        let mut pipeline = PlaybackPipeline::default();

        assert!(pipeline.video_present_queue_is_empty());
        assert_eq!(pipeline.video_present_queue_len(), 0);

        pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(16), 1));
        pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(33), 2));

        assert_eq!(pipeline.video_present_queue_len(), 2);
        assert_eq!(
            pipeline.front_queued_video_frame().map(|frame| frame.pts),
            Some(Duration::from_millis(16))
        );
        assert_eq!(
            pipeline
                .front_and_next_queued_video_frames()
                .map(|(front_frame, next_frame)| (front_frame.pts, next_frame.pts)),
            Some((Duration::from_millis(16), Duration::from_millis(33)))
        );

        assert_eq!(
            pipeline
                .pop_queued_video_frame_front()
                .map(|frame| frame.texture_handle),
            Some(video_core::FrameTextureHandle(1))
        );
        assert_eq!(
            pipeline
                .pop_queued_video_frame_front()
                .map(|frame| frame.texture_handle),
            Some(video_core::FrameTextureHandle(2))
        );
        assert!(pipeline.pop_queued_video_frame_front().is_none());
        assert!(pipeline.video_present_queue_is_empty());
    }

    #[test]
    fn present_video_frame_methods_keep_replacement_ownership_explicit() {
        let mut pipeline = PlaybackPipeline::default();

        assert!(!pipeline.has_present_video_frame());
        assert!(pipeline.present_video_frame().is_none());
        assert!(pipeline.take_present_video_frame().is_none());

        pipeline.set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(10), 10));

        assert!(pipeline.has_present_video_frame());
        assert_eq!(
            pipeline.present_video_frame().map(|frame| frame.pts),
            Some(Duration::from_millis(10))
        );

        let replaced_frame = pipeline
            .replace_present_video_frame(decoded_frame_for_tests(Duration::from_millis(20), 20));

        assert_eq!(
            replaced_frame.map(|frame| frame.texture_handle),
            Some(video_core::FrameTextureHandle(10))
        );
        assert_eq!(
            pipeline
                .take_present_video_frame()
                .map(|frame| frame.texture_handle),
            Some(video_core::FrameTextureHandle(20))
        );
        assert!(!pipeline.has_present_video_frame());
    }

    #[test]
    fn opened_media_boundary_methods_expose_installed_source_slots() {
        let mut pipeline = PlaybackPipeline::default();
        let track_infos = vec![
            source_slot_track(TrackId::new(1), TrackKind::Video, "V_VP9"),
            source_slot_track(TrackId::new(2), TrackKind::Audio, "A_OPUS"),
        ];

        assert!(!pipeline.has_demuxer());
        assert_eq!(pipeline.track_count(), 0);

        pipeline.install_opened_media(
            Box::new(SourceSlotFakeDemuxer::new(track_infos.clone())),
            Some(PathBuf::from("/tmp/source.webm")),
            Some("external source".to_owned()),
            track_infos,
        );

        assert!(pipeline.has_demuxer());
        assert_eq!(
            pipeline.source_file_path(),
            Some(Path::new("/tmp/source.webm"))
        );
        assert_eq!(pipeline.source_label(), Some("external source"));
        assert_eq!(pipeline.track_count(), 2);
        assert_eq!(pipeline.tracks()[0].id, TrackId::new(1));
        assert_eq!(pipeline.tracks()[1].kind, TrackKind::Audio);
    }

    #[test]
    fn demux_boundaries_preserve_eof_and_seek_results() {
        let mut pipeline = PlaybackPipeline::default();
        pipeline.install_opened_media(
            Box::new(SourceSlotFakeDemuxer::new(Vec::new())),
            None,
            None,
            Vec::new(),
        );

        let packet_result = pipeline
            .demux_next_packet()
            .expect("installed demuxer должен быть видим через boundary")
            .expect("fake demuxer не должен возвращать ошибку");
        assert!(packet_result.is_none());

        let seek_result = pipeline
            .seek_demuxer(DemuxSeekRequest::accurate(Duration::from_secs(3)))
            .expect("installed demuxer должен принять seek через boundary")
            .expect("fake demuxer должен принять accurate seek");
        assert_eq!(seek_result.actual_position, MediaTime::from_secs(3));
    }

    #[test]
    fn clear_video_queues_returns_only_queued_texture_handles() {
        let mut pipeline = PlaybackPipeline::default();

        pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(16), 1));
        pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(33), 2));
        pipeline.set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(50), 3));
        pipeline.replace_seek_preroll_fallback_video_frame(decoded_frame_for_tests(
            Duration::from_millis(40),
            4,
        ));

        let released_texture_handles = pipeline.clear_video_queues();

        assert_eq!(
            released_texture_handles,
            vec![
                video_core::FrameTextureHandle(1),
                video_core::FrameTextureHandle(2)
            ]
        );
        assert!(pipeline.video_present_queue_is_empty());
        assert_eq!(
            pipeline
                .present_video_frame()
                .map(|frame| frame.texture_handle),
            Some(video_core::FrameTextureHandle(3))
        );
        assert_eq!(
            pipeline
                .take_seek_preroll_fallback_video_frame()
                .map(|frame| frame.texture_handle),
            Some(video_core::FrameTextureHandle(4))
        );
    }

    /// Fake demuxer для проверки source-slot boundaries без реального container backend-а.
    struct SourceSlotFakeDemuxer {
        /// Metadata tracks, которые demuxer отдаёт по neutral contract.
        track_infos: Vec<TrackInfo>,
    }

    impl SourceSlotFakeDemuxer {
        /// Создаёт fake demuxer с фиксированным набором tracks.
        fn new(track_infos: Vec<TrackInfo>) -> Self {
            Self { track_infos }
        }
    }

    impl Demuxer for SourceSlotFakeDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &self.track_infos
        }

        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(30))
        }

        fn next_packet(&mut self) -> anyhow::Result<Option<media_core::Packet>> {
            Ok(None)
        }

        fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            Ok(DemuxSeekResult {
                requested_position: MediaTime::from_duration(timestamp),
                actual_position: MediaTime::from_duration(timestamp),
                actual_track_timestamp: None,
            })
        }
    }

    /// Создаёт минимальный track metadata для проверки source-slot getters.
    fn source_slot_track(track_id: TrackId, kind: TrackKind, codec_id: &str) -> TrackInfo {
        TrackInfo {
            id: track_id,
            kind,
            codec_id: codec_id.to_owned(),
            codec_private: None,
            time_base: media_core::TimeBase::new(1, 1_000),
            duration: Some(Duration::from_secs(30)),
            sample_rate: (kind == TrackKind::Audio).then_some(48_000),
            channels: (kind == TrackKind::Audio).then_some(2),
            video: None,
        }
    }
}
