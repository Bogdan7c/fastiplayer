use super::*;

// Общие test doubles и builders остаются в одном месте, чтобы доменные тесты
// проверяли поведение `PlayerSession`, а не дублировали setup-код.
/// Собирает абсолютный scrub request с явным seek mode для проверки boundary state.
pub(super) fn session_scrub_request(seconds: u64, mode: SeekMode) -> SeekRequest {
    SeekRequest {
        target: SeekTarget::Absolute(MediaTime::from_secs(seconds)),
        mode,
    }
}

/// Создаёт минимальный seek commit для чистых audio-gate тестов.
pub(super) fn audio_gate_seek_commit(resume_intent: PlaybackResumeIntent) -> SeekCommitState {
    SeekCommitState {
        generation: 4,
        target_position: MediaTime::from_secs(8),
        actual_position: MediaTime::from_secs(8),
        started_at: Instant::now(),
        resume_intent,
    }
}

/// Fake demuxer для проверки player-core transaction без реального WebM/GPU.
pub(super) struct FakeDemuxer {
    pub(super) tracks: Vec<TrackInfo>,
    pub(super) duration: Option<Duration>,
    pub(super) packets: VecDeque<media_core::Packet>,
    pub(super) events: VecDeque<DemuxReadEvent>,
    pub(super) seek_log: Arc<Mutex<Vec<Duration>>>,
    pub(super) seek_request_log: Option<Arc<Mutex<Vec<DemuxSeekRequest>>>>,
    pub(super) event_read_count: Option<Arc<AtomicUsize>>,
    pub(super) seek_results: VecDeque<DemuxSeekResult>,
    pub(super) seekability: DemuxSeekability,
}

impl FakeDemuxer {
    /// Создаёт fake demuxer с явными tracks и shared seek log.
    pub(super) fn new(
        tracks: Vec<TrackInfo>,
        duration: Option<Duration>,
        seek_log: Arc<Mutex<Vec<Duration>>>,
    ) -> Self {
        Self {
            tracks,
            duration,
            packets: VecDeque::new(),
            events: VecDeque::new(),
            seek_log,
            seek_request_log: None,
            event_read_count: None,
            seek_results: VecDeque::new(),
            seekability: DemuxSeekability::Seekable,
        }
    }

    /// Задаёт seekability для сценариев с playback-only source.
    pub(super) fn with_seekability(mut self, seekability: DemuxSeekability) -> Self {
        self.seekability = seekability;
        self
    }

    /// Подключает отдельный log полных demux seek request-ов.
    pub(super) fn with_seek_request_log(
        mut self,
        seek_request_log: Arc<Mutex<Vec<DemuxSeekRequest>>>,
    ) -> Self {
        self.seek_request_log = Some(seek_request_log);
        self
    }

    /// Подключает счётчик `next_event`, чтобы EOF tests проверяли отсутствие новых demux reads.
    pub(super) fn with_event_read_count(mut self, event_read_count: Arc<AtomicUsize>) -> Self {
        self.event_read_count = Some(event_read_count);
        self
    }

    /// Добавляет scripted seek result, чтобы тест мог отделить requested target от actual demux point.
    pub(super) fn with_seek_result(mut self, seek_result: DemuxSeekResult) -> Self {
        self.seek_results.push_back(seek_result);
        self
    }

    /// Добавляет packet в fake demux stream без раскрытия внутренней очереди тестам.
    pub(super) fn push_packet(&mut self, packet: media_core::Packet) {
        self.packets.push_back(packet);
    }

    /// Добавляет lifecycle/packet event в fake demux stream.
    pub(super) fn push_event(&mut self, event: DemuxReadEvent) {
        self.events.push_back(event);
    }
}

impl Demuxer for FakeDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn seekability(&self) -> DemuxSeekability {
        self.seekability
    }

    fn next_packet(&mut self) -> anyhow::Result<Option<media_core::Packet>> {
        Ok(self.packets.pop_front())
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        if let Some(ref event_read_count) = self.event_read_count {
            event_read_count.fetch_add(1, Ordering::Relaxed);
        }

        let Some(event) = self.events.pop_front() else {
            return match self.next_packet()? {
                Some(packet) => Ok(DemuxReadEvent::Packet(packet)),
                None => Ok(DemuxReadEvent::EndOfStream),
            };
        };

        if let DemuxReadEvent::TracksChanged(ref track_update) = event {
            self.tracks = track_update.tracks.clone();
            self.duration = track_update.duration;
        }

        Ok(event)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_log
            .lock()
            .expect("seek log mutex should not be poisoned")
            .push(timestamp);

        Ok(self.seek_results.pop_front().unwrap_or(DemuxSeekResult {
            requested_position: MediaTime::from_duration(timestamp),
            actual_position: MediaTime::from_duration(timestamp),
            actual_track_timestamp: None,
        }))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        if let Some(ref seek_request_log) = self.seek_request_log {
            seek_request_log
                .lock()
                .expect("seek request log mutex should not be poisoned")
                .push(request);
        }

        self.seek(request.timestamp)
    }
}

/// Fake decoder thread, который нужен только для проверки failed flush boundary.
pub(super) struct FailingFlushVideoDecoderThread {
    /// Текст ошибки, возвращаемый из session-level decoder reset.
    pub(super) error_message: &'static str,
}

impl FailingFlushVideoDecoderThread {
    /// Создаёт fake decoder thread с предсказуемой flush ошибкой.
    pub(super) const fn new(error_message: &'static str) -> Self {
        Self { error_message }
    }
}

impl video_core::VideoDecoderThreadHandle for FailingFlushVideoDecoderThread {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        "Fake failing decoder"
    }

    fn send_packet(&self, _packet: PlayerDecodePacket) -> Result<(), DecodeSendError> {
        Err(DecodeSendError::Fatal(DecodeThreadError::new(
            "fake decoder cannot accept packets",
        )))
    }

    fn release_frame(&self, _handle: video_core::FrameTextureHandle) {}

    fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
        None
    }

    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        None
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        None
    }

    fn flush(&self) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("{}", self.error_message))
    }

    fn resource_provider(&self) -> PresentFrameResourceProviderHandle {
        panic!("fake failing decoder does not provide renderer resources")
    }

    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        None
    }

    fn packet_queue_depth(&self) -> usize {
        0
    }

    fn drain_completed_packet_count(&self) -> usize {
        0
    }
}

/// Fake audio decoder считает reset-вызовы без CPAL и codec side effects.
pub(super) struct CountingAudioDecoder {
    /// Shared counter reset-вызовов.
    pub(super) reset_count: Arc<AtomicUsize>,
}

impl CountingAudioDecoder {
    /// Создаёт fake decoder с внешним счётчиком.
    pub(super) fn new(reset_count: Arc<AtomicUsize>) -> Self {
        Self { reset_count }
    }
}

impl audio_core::AudioDecoder for CountingAudioDecoder {
    /// Decode в этом fake path-е не используется.
    fn decode(&mut self, _packet: &audio_core::EncodedAudioPacket<'_>) -> anyhow::Result<Vec<f32>> {
        Ok(Vec::new())
    }

    /// Отмечает reset после seek.
    fn reset(&mut self) -> anyhow::Result<()> {
        self.reset_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Возвращает стабильный sample rate для boundary contract-а.
    fn sample_rate(&self) -> u32 {
        48_000
    }

    /// Возвращает стабильный channel count для boundary contract-а.
    fn channels(&self) -> u32 {
        2
    }
}

/// Упаковывает fake decoder в neutral trait object для тестов boundary methods.
pub(super) fn counting_audio_decoder_handle(
    reset_count: Arc<AtomicUsize>,
) -> audio_core::AudioDecoderHandle {
    Box::new(CountingAudioDecoder::new(reset_count))
}

/// Наблюдаемый handle fake decoder factory для lazy-init tests.
#[derive(Clone)]
pub(super) struct ScriptedAudioDecoderFactoryHandle {
    /// Config-и, по которым session реально запросила decoder.
    pub(super) created_configs: Arc<Mutex<Vec<audio_core::AudioDecoderConfig>>>,
}

impl ScriptedAudioDecoderFactoryHandle {
    /// Создаёт пустой журнал factory calls.
    pub(super) fn new() -> Self {
        Self {
            created_configs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Возвращает снимок созданных decoder config-ов.
    pub(super) fn created_configs(&self) -> Vec<audio_core::AudioDecoderConfig> {
        self.created_configs
            .lock()
            .expect("scripted audio decoder factory log lock")
            .clone()
    }
}

/// Явный сценарий fake decoder factory без concrete audio crate.
pub(super) enum ScriptedAudioDecoderFactoryOutcome {
    /// Успешно вернуть decoder, который decode-ит пустой PCM packet.
    Success,

    /// Вернуть stable typed unsupported codec error.
    UnsupportedCodec,

    /// Вернуть generic init failure.
    Error(&'static str),
}

/// Fake decoder factory для проверки lazy creation policy в player-core.
pub(super) struct ScriptedAudioDecoderFactory {
    /// Журнал вызовов, доступный тесту после packet processing.
    pub(super) handle: ScriptedAudioDecoderFactoryHandle,

    /// Предсказуемый результат factory call-а.
    pub(super) outcome: ScriptedAudioDecoderFactoryOutcome,
}

impl ScriptedAudioDecoderFactory {
    /// Создаёт factory, которая успешно возвращает fake decoder.
    pub(super) fn success() -> (Arc<Self>, ScriptedAudioDecoderFactoryHandle) {
        Self::new(ScriptedAudioDecoderFactoryOutcome::Success)
    }

    /// Создаёт factory, которая возвращает unsupported codec.
    pub(super) fn unsupported_codec() -> (Arc<Self>, ScriptedAudioDecoderFactoryHandle) {
        Self::new(ScriptedAudioDecoderFactoryOutcome::UnsupportedCodec)
    }

    /// Создаёт factory, которая возвращает generic init error.
    pub(super) fn error(error: &'static str) -> (Arc<Self>, ScriptedAudioDecoderFactoryHandle) {
        Self::new(ScriptedAudioDecoderFactoryOutcome::Error(error))
    }

    /// Создаёт scripted factory с общим журналом вызовов.
    pub(super) fn new(
        outcome: ScriptedAudioDecoderFactoryOutcome,
    ) -> (Arc<Self>, ScriptedAudioDecoderFactoryHandle) {
        let handle = ScriptedAudioDecoderFactoryHandle::new();
        (
            Arc::new(Self {
                handle: handle.clone(),
                outcome,
            }),
            handle,
        )
    }
}

impl audio_core::AudioDecoderFactory for ScriptedAudioDecoderFactory {
    /// Записывает config и возвращает scripted outcome без Symphonia/Opus deps.
    fn create_decoder(
        &self,
        config: audio_core::AudioDecoderConfig,
    ) -> anyhow::Result<audio_core::AudioDecoderHandle> {
        self.handle
            .created_configs
            .lock()
            .expect("scripted audio decoder factory log lock")
            .push(config.clone());

        match self.outcome {
            ScriptedAudioDecoderFactoryOutcome::Success => Ok(Box::new(CountingAudioDecoder::new(
                Arc::new(AtomicUsize::new(0)),
            ))),
            ScriptedAudioDecoderFactoryOutcome::UnsupportedCodec => {
                Err(audio_core::AudioDecoderError::UnsupportedCodec {
                    codec_id: config.codec_id().to_string(),
                }
                .into())
            }
            ScriptedAudioDecoderFactoryOutcome::Error(error) => Err(anyhow::anyhow!(error)),
        }
    }
}

/// Fake clock для session tests без concrete audio crate clock.
pub(super) struct ScriptedAudioClock {
    /// Текущая playback позиция, которую увидит session.
    pub(super) now: Mutex<Duration>,

    /// Количество reset-вызовов через neutral boundary.
    pub(super) reset_count: AtomicUsize,

    /// Scripted underrun callbacks для diagnostics path.
    pub(super) underrun_callbacks: AtomicU64,
}

impl ScriptedAudioClock {
    /// Создаёт clock с нулевой позицией и без underrun callbacks.
    pub(super) fn new() -> Self {
        Self {
            now: Mutex::new(Duration::ZERO),
            reset_count: AtomicUsize::new(0),
            underrun_callbacks: AtomicU64::new(0),
        }
    }

    /// Возвращает trait-object handle для установки в pipeline.
    pub(super) fn as_player_clock(self: &Arc<Self>) -> Arc<dyn PlayerAudioClock> {
        let clock: Arc<dyn PlayerAudioClock> = self.clone();
        clock
    }

    /// Имитирует progress production clock-а для тестов seek/EOF gates.
    pub(super) fn record_played(&self, samples: u64) {
        let frames = samples / 2;
        let nanos = frames.saturating_mul(1_000_000_000) / 48_000;
        *self
            .now
            .lock()
            .expect("fake clock mutex не должен ломаться") = Duration::from_nanos(nanos);
    }
}

impl PlayerAudioClock for ScriptedAudioClock {
    /// Возвращает scripted playback позицию.
    fn now(&self) -> Duration {
        *self
            .now
            .lock()
            .expect("fake clock mutex не должен ломаться")
    }

    /// Сбрасывает позицию и отмечает reset.
    fn reset(&self) {
        self.reset_count.fetch_add(1, Ordering::Relaxed);
        *self
            .now
            .lock()
            .expect("fake clock mutex не должен ломаться") = Duration::ZERO;
    }

    /// Возвращает scripted underrun callbacks.
    fn underrun_callbacks(&self) -> u64 {
        self.underrun_callbacks.load(Ordering::Relaxed)
    }
}

/// Shared handle fake audio output-а для assertions после передачи output-а в pipeline.
#[derive(Clone)]
pub(super) struct ScriptedAudioOutputHandle {
    /// Clock fake output-а, который session может установить в pipeline.
    pub(super) clock: Arc<ScriptedAudioClock>,

    /// Shared уровень buffer-а, видимый через output boundary.
    pub(super) buffer_level_ms: Arc<Mutex<f64>>,

    /// Сколько раз session попросила запустить stream.
    pub(super) play_count: Arc<AtomicUsize>,

    /// Сколько раз session попросила поставить stream на паузу.
    pub(super) pause_count: Arc<AtomicUsize>,

    /// Сколько раз session очистила audio buffer для seek.
    pub(super) clear_count: Arc<AtomicUsize>,
}

impl ScriptedAudioOutputHandle {
    /// Создаёт shared buffer level, empty counters и clock с production-like sample layout.
    pub(super) fn new(buffer_level_ms: f64) -> Self {
        Self {
            clock: Arc::new(ScriptedAudioClock::new()),
            buffer_level_ms: Arc::new(Mutex::new(buffer_level_ms)),
            play_count: Arc::new(AtomicUsize::new(0)),
            pause_count: Arc::new(AtomicUsize::new(0)),
            clear_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Обновляет scripted buffer level, имитируя заполнение audio output-а.
    pub(super) fn set_buffer_level_ms(&self, buffer_level_ms: f64) {
        *self
            .buffer_level_ms
            .lock()
            .expect("audio buffer level mutex should not be poisoned") = buffer_level_ms;
    }

    /// Возвращает scripted buffer level без раскрытия mutex-а тестам.
    pub(super) fn buffer_level_ms(&self) -> f64 {
        *self
            .buffer_level_ms
            .lock()
            .expect("audio buffer level mutex should not be poisoned")
    }

    /// Возвращает neutral clock handle для установки в pipeline/assertions.
    pub(super) fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        self.clock.as_player_clock()
    }
}

/// Fake audio output для seek resume тестов без CPAL/device side effects.
pub(super) struct ScriptedAudioOutput {
    /// Shared counters и clock, доступные тесту после move в pipeline.
    pub(super) handle: ScriptedAudioOutputHandle,

    /// Ошибка, которую fake вернёт из play, если сценарий её задаёт.
    pub(super) play_error: Option<&'static str>,
}

impl ScriptedAudioOutput {
    /// Создаёт fake output с фиксированным buffer level и scripted play result.
    pub(super) fn new(buffer_level_ms: f64, play_error: Option<&'static str>) -> Self {
        Self {
            handle: ScriptedAudioOutputHandle::new(buffer_level_ms),
            play_error,
        }
    }

    /// Разделяет output и assertion handle перед установкой в pipeline.
    pub(super) fn into_parts(self) -> (Box<dyn PlayerAudioOutput>, ScriptedAudioOutputHandle) {
        let handle = self.handle.clone();

        (Box::new(self), handle)
    }
}

impl PlayerAudioOutput for ScriptedAudioOutput {
    /// Fake принимает все samples и возвращает их количество как written count.
    fn write_samples(&mut self, samples: &[f32]) -> u64 {
        samples.len() as u64
    }

    /// Записывает play attempt и возвращает scripted success/error.
    fn play(&mut self) -> anyhow::Result<()> {
        self.handle.play_count.fetch_add(1, Ordering::Relaxed);
        match self.play_error {
            Some(error) => Err(anyhow::anyhow!(error)),
            None => Ok(()),
        }
    }

    /// Записывает pause attempt; seek pause в этих тестах всегда успешен.
    fn pause(&mut self) -> anyhow::Result<()> {
        self.handle.pause_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Подтверждает очистку buffer-а ровно тем generation, который запросила session.
    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        self.handle.clear_count.fetch_add(1, Ordering::Relaxed);
        Ok(generation)
    }

    /// Volume не влияет на seek gate, поэтому fake только принимает boundary call.
    fn set_volume(&mut self, _volume: f32) {}

    /// Возвращает scripted buffer level для audio preroll gate.
    fn buffer_level_ms(&self) -> f64 {
        self.handle.buffer_level_ms()
    }

    /// Возвращает fake clock через neutral contract.
    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        self.handle.clock()
    }
}

/// Shared state factory fake-а для проверок lazy output init.
#[derive(Clone)]
pub(super) struct ScriptedAudioOutputFactoryHandle {
    /// Сколько раз session вызвала factory.
    pub(super) create_count: Arc<AtomicUsize>,

    /// Specs, с которыми session просила создать output.
    pub(super) created_specs: Arc<Mutex<Vec<AudioOutputSpec>>>,

    /// Последний output handle, созданный factory.
    pub(super) last_output_handle: Arc<Mutex<Option<ScriptedAudioOutputHandle>>>,
}

impl ScriptedAudioOutputFactoryHandle {
    /// Создаёт пустой assertion handle для factory.
    pub(super) fn new() -> Self {
        Self {
            create_count: Arc::new(AtomicUsize::new(0)),
            created_specs: Arc::new(Mutex::new(Vec::new())),
            last_output_handle: Arc::new(Mutex::new(None)),
        }
    }

    /// Возвращает количество create-вызовов.
    pub(super) fn create_count(&self) -> usize {
        self.create_count.load(Ordering::Relaxed)
    }

    /// Возвращает specs, полученные fake factory.
    pub(super) fn created_specs(&self) -> Vec<AudioOutputSpec> {
        self.created_specs
            .lock()
            .expect("factory specs mutex не должен ломаться")
            .clone()
    }

    /// Возвращает handle последнего output-а, если factory создала output.
    pub(super) fn last_output_handle(&self) -> Option<ScriptedAudioOutputHandle> {
        self.last_output_handle
            .lock()
            .expect("factory output mutex не должен ломаться")
            .clone()
    }
}

/// Fake factory, возвращающая scripted output или creation error без CPAL.
pub(super) struct ScriptedAudioOutputFactory {
    /// Shared assertions factory-а.
    pub(super) handle: ScriptedAudioOutputFactoryHandle,

    /// Ошибка создания output-а, если сценарий проверяет failure path.
    pub(super) creation_error: Option<&'static str>,

    /// Scripted buffer level создаваемого output-а.
    pub(super) buffer_level_ms: f64,

    /// Scripted play error создаваемого output-а.
    pub(super) play_error: Option<&'static str>,
}

impl ScriptedAudioOutputFactory {
    /// Создаёт successful factory с заданным output behavior.
    pub(super) fn success(
        buffer_level_ms: f64,
        play_error: Option<&'static str>,
    ) -> (Arc<Self>, ScriptedAudioOutputFactoryHandle) {
        Self::new(buffer_level_ms, play_error, None)
    }

    /// Создаёт failing factory, имитирующую ошибку устройства.
    pub(super) fn failure(error: &'static str) -> (Arc<Self>, ScriptedAudioOutputFactoryHandle) {
        Self::new(0.0, None, Some(error))
    }

    /// Общий constructor для success/failure сценариев.
    pub(super) fn new(
        buffer_level_ms: f64,
        play_error: Option<&'static str>,
        creation_error: Option<&'static str>,
    ) -> (Arc<Self>, ScriptedAudioOutputFactoryHandle) {
        let handle = ScriptedAudioOutputFactoryHandle::new();
        let factory = Arc::new(Self {
            handle: handle.clone(),
            creation_error,
            buffer_level_ms,
            play_error,
        });

        (factory, handle)
    }
}

impl AudioOutputFactory for ScriptedAudioOutputFactory {
    /// Создаёт scripted output и записывает spec, чтобы тест проверил lazy boundary call.
    fn create_output(&self, spec: AudioOutputSpec) -> anyhow::Result<Box<dyn PlayerAudioOutput>> {
        self.handle.create_count.fetch_add(1, Ordering::Relaxed);
        self.handle
            .created_specs
            .lock()
            .expect("factory specs mutex не должен ломаться")
            .push(spec);

        if let Some(error) = self.creation_error {
            anyhow::bail!(error);
        }

        let output = ScriptedAudioOutput::new(self.buffer_level_ms, self.play_error);
        let (output, output_handle) = output.into_parts();
        *self
            .handle
            .last_output_handle
            .lock()
            .expect("factory output mutex не должен ломаться") = Some(output_handle);

        Ok(output)
    }
}

/// Устанавливает decoder/output/clock так, чтобы audio gate мог быть Ready.
pub(super) fn install_ready_audio_runtime(
    session: &mut PlayerSession,
    buffer_level_ms: f64,
    play_error: Option<&'static str>,
) -> ScriptedAudioOutputHandle {
    let reset_count = Arc::new(AtomicUsize::new(0));
    session
        .pipeline
        .install_audio_decoder(counting_audio_decoder_handle(reset_count));

    let (audio_output, handle) = ScriptedAudioOutput::new(buffer_level_ms, play_error).into_parts();
    session
        .pipeline
        .install_audio_output_for_tests(audio_output);
    session.pipeline.install_audio_clock(handle.clock());

    handle
}

/// Shared fake decoder для seek/tick тестов без production decoder и renderer resources.
#[derive(Clone)]
pub(super) struct SharedFakeVideoDecoderThread {
    /// Очередь decoded frames, которые fake decoder отдаёт через `try_recv_frame`.
    pub(super) decoded_frames: Arc<Mutex<VecDeque<video_core::DecodedFrame>>>,

    /// Очередь результатов `send_packet` для проверки decoder boundary без реального backend-а.
    pub(super) send_results: Arc<Mutex<VecDeque<Result<(), DecodeSendError>>>>,

    /// Packet log нужен тестам, чтобы отличать stale-generation drop от отправки в decoder.
    pub(super) sent_packets: Arc<Mutex<Vec<PlayerDecodePacket>>>,

    /// Накопленные packet ack-и, которые fake decoder отдаёт одним drain-вызовом.
    pub(super) completed_packet_count: Arc<Mutex<usize>>,

    /// Scripted frames, которые fake decoder публикует сразу после успешной отправки packet-а.
    pub(super) decode_on_send_frames: Arc<Mutex<VecDeque<(Duration, u64)>>>,

    /// Diagnostics events, которые fake decoder отдаёт через boundary drain.
    pub(super) diagnostic_events: Arc<Mutex<VecDeque<video_core::VideoDecoderDiagnosticEvent>>>,

    /// Опциональная flush ошибка для проверки проброса seek/reset failure.
    pub(super) flush_error_message: Arc<Mutex<Option<String>>>,

    /// Texture handles, которые session вернула decoder boundary через release.
    pub(super) released_handles: Arc<Mutex<Vec<video_core::FrameTextureHandle>>>,

    /// Snapshot texture/surface pressure, который fake отдаёт scheduler-у.
    pub(super) resource_snapshot: Arc<Mutex<Option<DecoderResourceSnapshot>>>,

    /// Control-channel pressure snapshot, который fake отдаёт через decoder boundary.
    pub(super) control_pressure: Arc<Mutex<Option<DecoderControlChannelPressureSnapshot>>>,
}

impl SharedFakeVideoDecoderThread {
    /// Создаёт fake decoder с пустыми output/release очередями.
    pub(super) fn new() -> Self {
        Self {
            decoded_frames: Arc::new(Mutex::new(VecDeque::new())),
            send_results: Arc::new(Mutex::new(VecDeque::new())),
            sent_packets: Arc::new(Mutex::new(Vec::new())),
            completed_packet_count: Arc::new(Mutex::new(0)),
            decode_on_send_frames: Arc::new(Mutex::new(VecDeque::new())),
            diagnostic_events: Arc::new(Mutex::new(VecDeque::new())),
            flush_error_message: Arc::new(Mutex::new(None)),
            released_handles: Arc::new(Mutex::new(Vec::new())),
            resource_snapshot: Arc::new(Mutex::new(None)),
            control_pressure: Arc::new(Mutex::new(None)),
        }
    }

    /// Публикует frame так, как production decoder thread сделал бы после seek decode.
    pub(super) fn push_decoded_frame(&self, mut frame: video_core::DecodedFrame) {
        if frame.generation == 0 {
            if let Some(generation) = self
                .sent_packets
                .lock()
                .expect("fake decoder sent packet log lock")
                .last()
                .map(|packet| packet.generation)
            {
                frame.generation = generation;
            }
        }

        self.decoded_frames
            .lock()
            .expect("fake decoder frame queue lock")
            .push_back(frame);
    }

    /// Добавляет предсказуемый результат отправки packet-а через fake boundary.
    pub(super) fn push_send_result(&self, result: Result<(), DecodeSendError>) {
        self.send_results
            .lock()
            .expect("fake decoder send result queue lock")
            .push_back(result);
    }

    /// Возвращает snapshot packets, принятых fake decoder boundary.
    pub(super) fn sent_packets(&self) -> Vec<PlayerDecodePacket> {
        self.sent_packets
            .lock()
            .expect("fake decoder sent packet log lock")
            .clone()
    }

    /// Публикует packet ack-и так, как production decoder сделал бы после decode/flush drain.
    pub(super) fn add_completed_packet_count(&self, packet_count: usize) {
        let mut completed_packet_count = self
            .completed_packet_count
            .lock()
            .expect("fake decoder ack counter lock");
        *completed_packet_count = completed_packet_count.saturating_add(packet_count);
    }

    /// Настраивает следующий успешно отправленный packet так, чтобы fake сразу отдал frame и ACK.
    pub(super) fn decode_next_packet_as_frame(&self, frame_pts: Duration, frame_handle: u64) {
        self.decode_on_send_frames
            .lock()
            .expect("fake decoder decode-on-send queue lock")
            .push_back((frame_pts, frame_handle));
    }

    /// Публикует diagnostics event так, как production decoder boundary сделал бы.
    pub(super) fn push_diagnostic_event(&self, event: video_core::VideoDecoderDiagnosticEvent) {
        self.diagnostic_events
            .lock()
            .expect("fake decoder diagnostics queue lock")
            .push_back(event);
    }

    /// Настраивает следующую flush ошибку без изменения decoder lifecycle.
    pub(super) fn fail_flush_with(&self, error_message: &str) {
        *self
            .flush_error_message
            .lock()
            .expect("fake decoder flush error lock") = Some(error_message.to_string());
    }

    /// Возвращает release log для проверки bounded queue и ownership.
    pub(super) fn released_handles(&self) -> Vec<video_core::FrameTextureHandle> {
        self.released_handles
            .lock()
            .expect("fake decoder release log lock")
            .clone()
    }

    /// Настраивает resource snapshot для texture pressure тестов.
    pub(super) fn set_resource_snapshot(&self, resource_snapshot: DecoderResourceSnapshot) {
        *self
            .resource_snapshot
            .lock()
            .expect("fake decoder resource snapshot lock") = Some(resource_snapshot);
    }

    /// Настраивает control-channel pressure snapshot для проверки pipeline boundary.
    pub(super) fn set_control_pressure(&self, pressure: DecoderControlChannelPressureSnapshot) {
        *self
            .control_pressure
            .lock()
            .expect("fake decoder control pressure lock") = Some(pressure);
    }
}

impl video_core::VideoDecoderThreadHandle for SharedFakeVideoDecoderThread {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        "Shared fake decoder"
    }

    fn send_packet(&self, packet: PlayerDecodePacket) -> Result<(), DecodeSendError> {
        let send_result = self
            .send_results
            .lock()
            .expect("fake decoder send result queue lock")
            .pop_front()
            .unwrap_or(Ok(()));

        if send_result.is_ok() {
            let packet_generation = packet.generation;
            self.sent_packets
                .lock()
                .expect("fake decoder sent packet log lock")
                .push(packet);
            if let Some((frame_pts, frame_handle)) = self
                .decode_on_send_frames
                .lock()
                .expect("fake decoder decode-on-send queue lock")
                .pop_front()
            {
                let mut decoded_frame = decoded_frame_for_tests(frame_pts, frame_handle);
                decoded_frame.generation = packet_generation;
                self.decoded_frames
                    .lock()
                    .expect("fake decoder frame queue lock")
                    .push_back(decoded_frame);
                self.add_completed_packet_count(1);
            }
        }

        send_result
    }

    fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        self.released_handles
            .lock()
            .expect("fake decoder release log lock")
            .push(handle);

        if let Some(resource_snapshot) = self
            .resource_snapshot
            .lock()
            .expect("fake decoder resource snapshot lock")
            .as_mut()
        {
            resource_snapshot.in_use = resource_snapshot.in_use.saturating_sub(1);
            resource_snapshot.free_surfaces = resource_snapshot.free_surfaces.saturating_add(1);
        }
    }

    fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
        self.decoded_frames
            .lock()
            .expect("fake decoder frame queue lock")
            .pop_front()
    }

    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        self.diagnostic_events
            .lock()
            .expect("fake decoder diagnostics queue lock")
            .pop_front()
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        None
    }

    fn flush(&self) -> anyhow::Result<()> {
        if let Some(error_message) = self
            .flush_error_message
            .lock()
            .expect("fake decoder flush error lock")
            .take()
        {
            return Err(anyhow::anyhow!(error_message));
        }

        Ok(())
    }

    fn resource_provider(&self) -> PresentFrameResourceProviderHandle {
        panic!("shared fake decoder does not provide renderer resources")
    }

    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        *self
            .resource_snapshot
            .lock()
            .expect("fake decoder resource snapshot lock")
    }

    fn decoder_control_channel_pressure(&self) -> Option<DecoderControlChannelPressureSnapshot> {
        *self
            .control_pressure
            .lock()
            .expect("fake decoder control pressure lock")
    }

    fn packet_queue_depth(&self) -> usize {
        0
    }

    fn drain_completed_packet_count(&self) -> usize {
        let mut completed_packet_count = self
            .completed_packet_count
            .lock()
            .expect("fake decoder ack counter lock");
        let drained_packet_count = *completed_packet_count;
        *completed_packet_count = 0;
        drained_packet_count
    }
}

/// Создаёт минимальный track summary для fake media.
pub(super) fn fake_track(track_id: u32, kind: TrackKind) -> TrackInfo {
    TrackInfo {
        id: TrackId::new(track_id),
        kind,
        codec_id: match kind {
            TrackKind::Video => "V_VP9".to_string(),
            TrackKind::Audio => "A_OPUS".to_string(),
        },
        codec_private: None,
        time_base: media_core::TimeBase::new(1, 1_000),
        duration: Some(Duration::from_secs(30)),
        sample_rate: (kind == TrackKind::Audio).then_some(48_000),
        channels: (kind == TrackKind::Audio).then_some(2),
        video: None,
    }
}

/// Создаёт audio track с заданным container codec id для чистых codec selection tests.
pub(super) fn fake_audio_track_with_codec(track_id: u32, codec_id: &str) -> TrackInfo {
    let mut track = fake_track(track_id, TrackKind::Audio);
    track.codec_id = codec_id.to_string();
    track
}

/// Подключает fake demuxer без инициализации audio/video backend ресурсов.
pub(super) fn install_fake_media(
    session: &mut PlayerSession,
    tracks: Vec<TrackInfo>,
) -> Arc<Mutex<Vec<Duration>>> {
    install_fake_media_with_seekability(session, tracks, DemuxSeekability::Seekable)
}

/// Подключает fake demuxer с заданной seekability.
pub(super) fn install_fake_media_with_seekability(
    session: &mut PlayerSession,
    tracks: Vec<TrackInfo>,
    seekability: DemuxSeekability,
) -> Arc<Mutex<Vec<Duration>>> {
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let demuxer = FakeDemuxer::new(
        tracks.clone(),
        Some(Duration::from_secs(30)),
        Arc::clone(&seek_log),
    )
    .with_seekability(seekability);

    session
        .pipeline
        .install_opened_media(Box::new(demuxer), None, None, tracks.clone());
    if let Some(video_track_id) = tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .map(|track| track.id)
    {
        session
            .pipeline
            .select_video_track_preserving_active_requirement(video_track_id);
    }
    if let Some(audio_track_id) = tracks
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .map(|track| track.id)
    {
        session.pipeline.select_audio_track(audio_track_id);
    }
    session.set_snapshot_duration(Some(Duration::from_secs(30)));
    session.apply_demux_seekability(seekability);
    session.set_playback_state(PlaybackState::Paused);

    seek_log
}

/// Подключает fake demuxer и возвращает log полных demux seek request-ов.
pub(super) fn install_fake_media_with_seek_request_log(
    session: &mut PlayerSession,
    tracks: Vec<TrackInfo>,
) -> Arc<Mutex<Vec<DemuxSeekRequest>>> {
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let seek_request_log = Arc::new(Mutex::new(Vec::new()));
    let demuxer = FakeDemuxer::new(tracks.clone(), Some(Duration::from_secs(30)), seek_log)
        .with_seek_request_log(Arc::clone(&seek_request_log));

    session
        .pipeline
        .install_opened_media(Box::new(demuxer), None, None, tracks.clone());
    if let Some(video_track_id) = tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .map(|track| track.id)
    {
        session
            .pipeline
            .select_video_track_preserving_active_requirement(video_track_id);
    }
    if let Some(audio_track_id) = tracks
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .map(|track| track.id)
    {
        session.pipeline.select_audio_track(audio_track_id);
    }
    session.set_snapshot_duration(Some(Duration::from_secs(30)));
    session.apply_demux_seekability(DemuxSeekability::Seekable);
    session.set_playback_state(PlaybackState::Paused);

    seek_request_log
}

/// Возвращает индекс первого event-а, который подходит под проверку теста.
pub(super) fn event_index(
    events: &[PlayerEvent],
    matches_event: impl Fn(&PlayerEvent) -> bool,
    missing_message: &str,
) -> usize {
    events
        .iter()
        .position(matches_event)
        .unwrap_or_else(|| panic!("{missing_message}: {events:?}"))
}

/// Создаёт decoded frame без реальных GPU resources.
pub(super) fn decoded_frame_for_tests(pts: Duration, handle: u64) -> video_core::DecodedFrame {
    video_core::DecodedFrame {
        generation: 0,
        pts,
        format: video_core::DecodedPixelFormat::Nv12,
        bit_depth: BitDepth::Eight,
        chroma: ChromaSubsampling::Yuv420,
        memory_path: video_core::FrameMemoryPath::DmaBufZeroCopy,
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        color: codec_core::VideoColorMetadata::sdr_bt709_limited(),
        texture_handle: video_core::FrameTextureHandle(handle),
        diagnostics: video_core::VideoFrameDiagnostics::default(),
    }
}

/// Создаёт decoded frame, который принадлежит текущему seek generation session.
pub(super) fn decoded_frame_for_current_seek_generation(
    session: &PlayerSession,
    pts: Duration,
    handle: u64,
) -> video_core::DecodedFrame {
    let mut frame = decoded_frame_for_tests(pts, handle);
    frame.generation = session.pipeline.seek_generation();
    frame
}

/// Создаёт keyframe video packet для session-level demux/reset tests.
pub(super) fn fake_video_packet(track_id: TrackId, pts: Duration) -> media_core::Packet {
    media_core::Packet::new(
        track_id,
        TrackKind::Video,
        pts,
        None,
        true,
        Bytes::from_static(b"video-packet"),
    )
}

/// Создаёт video packet с явной keyframe-классификацией для seek regression scripts.
pub(super) fn fake_video_packet_with_keyframe(
    track_id: TrackId,
    pts: Duration,
    keyframe: PacketKeyframe,
) -> media_core::Packet {
    media_core::Packet::new_with_keyframe(
        track_id,
        TrackKind::Video,
        pts,
        None,
        keyframe,
        Bytes::from_static(b"video-packet"),
    )
}

/// Создаёт минимальный decode packet для проверки worker -> decoder boundary.
pub(super) fn decode_packet_for_tests(pts: Duration) -> PlayerDecodePacket {
    PlayerDecodePacket {
        track_id: TrackId::new(1),
        pts,
        generation: 0,
        encoded_bytes: Bytes::from_static(b"vp9-test-packet"),
        keyframe: true,
        resolved_color: None,
    }
}

/// Создаёт tick config с маленькой present queue для seek admission тестов.
pub(super) fn seek_admission_tick_config(
    max_video_present_queue: usize,
    max_decoded_video_frames_drained_per_tick: usize,
) -> PlayerTickConfig {
    PlayerTickConfig {
        max_video_present_queue,
        min_video_present_queue: 1,
        target_video_present_queue: 1,
        max_decoded_video_frames_drained_per_tick,
        seek_resume_video_min_ready_frames: 1,
        ..PlayerTickConfig::default()
    }
}

/// Собирает seek result, где container actual может быть раньше requested target.
pub(super) fn scripted_seek_result(requested: Duration, actual: Duration) -> DemuxSeekResult {
    DemuxSeekResult {
        requested_position: MediaTime::from_duration(requested),
        actual_position: MediaTime::from_duration(actual),
        actual_track_timestamp: None,
    }
}

/// Integration-style harness для seek/scrub regression tests без реальных media файлов.
pub(super) struct SeekRegressionHarness {
    /// Session остаётся владельцем seek state и pipeline инвариантов.
    pub(super) session: PlayerSession,

    /// Fake decoder boundary отдаёт decoded frames по явному сценарию теста.
    pub(super) decoder: SharedFakeVideoDecoderThread,

    /// Полные demux seek requests нужны для проверки mode/target без доступа к demuxer fields.
    pub(super) seek_request_log: Arc<Mutex<Vec<DemuxSeekRequest>>>,
}

impl SeekRegressionHarness {
    /// Подключает scripted demuxer и выбирает default audio/video tracks как media opening path.
    pub(super) fn new(tracks: Vec<TrackInfo>, demuxer: FakeDemuxer) -> Self {
        let mut session = PlayerSession::new();
        let decoder = SharedFakeVideoDecoderThread::new();
        let seek_request_log = demuxer
            .seek_request_log
            .as_ref()
            .expect("seek regression demuxer должен писать full request log")
            .clone();

        session
            .pipeline
            .install_opened_media(Box::new(demuxer), None, None, tracks.clone());
        if let Some(video_track_id) = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .map(|track| track.id)
        {
            session
                .pipeline
                .select_video_track_preserving_active_requirement(video_track_id);
            session.pipeline.set_video_decoder_thread(decoder.clone());
        }
        if let Some(audio_track_id) = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .map(|track| track.id)
        {
            session.pipeline.select_audio_track(audio_track_id);
        }
        session.set_snapshot_duration(Some(Duration::from_secs(30)));
        session.apply_demux_seekability(DemuxSeekability::Seekable);
        session.set_playback_state(PlaybackState::Paused);

        Self {
            session,
            decoder,
            seek_request_log,
        }
    }

    /// Выполняет один deterministic tick с маленькими бюджетами, чтобы сценарий шёл пошагово.
    pub(super) fn tick_once(&mut self) -> PlayerTickResult {
        self.session.tick(PlayerTickContext::with_config(
            Instant::now(),
            seek_regression_tick_config(),
        ))
    }

    /// Запускает final seek к absolute target через public command boundary.
    pub(super) fn start_final_seek(&mut self, target: MediaTime) {
        self.session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(target)))
            .expect("final seek command должен пройти session boundary");
    }

    /// Публикует decoded frame текущего packet generation и optionally закрывает in-flight packet.
    pub(super) fn push_decoded_frame(&self, pts: Duration, handle: u64, completed_packets: usize) {
        self.decoder
            .push_decoded_frame(decoded_frame_for_current_seek_generation(
                &self.session,
                pts,
                handle,
            ));
        if completed_packets > 0 {
            self.decoder.add_completed_packet_count(completed_packets);
        }
    }

    /// Возвращает активный seek и проверяет главный invariant generation alignment.
    pub(super) fn aligned_seek_commit(&self) -> SeekCommitState {
        let seek_commit = self
            .session
            .seek_commit()
            .expect("regression scenario должен иметь active seek commit");
        assert_eq!(
            seek_commit.generation,
            self.session.pipeline.seek_generation()
        );
        seek_commit
    }

    /// Возвращает packets, реально принятые decoder boundary.
    pub(super) fn sent_packets(&self) -> Vec<PlayerDecodePacket> {
        self.decoder.sent_packets()
    }

    /// Возвращает demux seek requests, реально отправленные session.
    pub(super) fn seek_requests(&self) -> Vec<DemuxSeekRequest> {
        self.seek_request_log
            .lock()
            .expect("seek regression request log lock")
            .clone()
    }
}

/// Конфиг tick-а для regression harness: один demux packet и один decoder send за шаг.
pub(super) fn seek_regression_tick_config() -> PlayerTickConfig {
    PlayerTickConfig {
        max_demux_packets_per_tick: 1,
        max_video_packets_sent_per_tick: 1,
        max_decoded_video_frames_drained_per_tick: 4,
        adaptive_catch_up_time_budget: Duration::ZERO,
        seek_commit_timeout: Duration::from_secs(10),
        ..seek_admission_tick_config(4, 4)
    }
}

/// Создаёт scripted fake demuxer с request log, actual seek point и post-seek packets.
pub(super) fn scripted_seek_demuxer(
    tracks: Vec<TrackInfo>,
    target: Duration,
    actual: Duration,
    packets: Vec<media_core::Packet>,
) -> FakeDemuxer {
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let seek_request_log = Arc::new(Mutex::new(Vec::new()));
    let mut demuxer = FakeDemuxer::new(tracks, Some(Duration::from_secs(30)), seek_log)
        .with_seek_request_log(seek_request_log)
        .with_seek_result(scripted_seek_result(target, actual));

    for packet in packets {
        demuxer.push_packet(packet);
    }

    demuxer
}

/// Создаёт backend-neutral texture pressure snapshot для seek admission тестов.
pub(super) fn decoder_resource_snapshot_for_tests(
    capacity: usize,
    in_use: usize,
) -> DecoderResourceSnapshot {
    DecoderResourceSnapshot {
        capacity,
        slots: capacity,
        in_use,
        free_surfaces: capacity.saturating_sub(in_use),
        waiting_gpu_completion: 0,
        waiting_decoder_reuse: 0,
        import_failures: 0,
        imports_created: 0,
        imports_reused: 0,
        imports_replaced: 0,
    }
}

pub(super) fn capabilities_with_vp9_profile0() -> SystemCapabilities {
    let backend_id = test_decode_backend_id();

    SystemCapabilities {
        schema_version: capability_core::CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id: backend_id.clone(),
            display_name: "Test hardware decoder".to_string(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            supported_video_decode_formats: vec![SupportedVideoDecodeFormat {
                codec: VideoCodec::Vp9,
                profile: VideoProfile::Vp9(Vp9Profile::Profile0),
                bit_depth: BitDepth::Eight,
                chroma: ChromaSubsampling::Yuv420,
                max_width: Some(1920),
                max_height: Some(1080),
                max_fps: None,
                hdr_input: false,
                backend: backend_id,
            }],
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            export_paths: vec![VideoExportPath::DmaBuf],
            p010_storage_layouts: Vec::new(),
            diagnostics: Vec::new(),
        }],
        render_backends: vec![RenderCapabilities::wgpu_nv12(Some(4096))],
    }
}

pub(super) fn capabilities_with_phase10_vp9_profile2_hdr() -> SystemCapabilities {
    let backend_id = test_decode_backend_id();

    SystemCapabilities {
        schema_version: capability_core::CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id: backend_id.clone(),
            display_name: "Test hardware decoder".to_string(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            supported_video_decode_formats: vec![SupportedVideoDecodeFormat {
                codec: VideoCodec::Vp9,
                profile: VideoProfile::Vp9(Vp9Profile::Profile2),
                bit_depth: BitDepth::Ten,
                chroma: ChromaSubsampling::Yuv420,
                max_width: Some(4096),
                max_height: Some(4096),
                max_fps: None,
                hdr_input: true,
                backend: backend_id,
            }],
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            export_paths: vec![VideoExportPath::DmaBuf],
            p010_storage_layouts: vec![P010StorageLayout::BaselineSeparateLayer],
            diagnostics: Vec::new(),
        }],
        render_backends: vec![RenderCapabilities::wgpu_p010_bt2446c(Some(4096))],
    }
}

pub(super) fn test_decode_backend_id() -> DecodeBackendId {
    DecodeBackendId::new("test_hardware_decoder")
        .expect("test backend id must use codec-core backend id syntax")
}

pub(super) fn bt2020_pq_limited() -> codec_core::VideoColorMetadata {
    codec_core::VideoColorMetadata::container(
        ColorRange::Limited,
        MatrixCoefficients::Bt2020,
        ColorPrimaries::Bt2020,
        TransferFunction::Pq,
        None,
    )
}
