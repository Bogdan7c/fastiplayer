//! Стенд замера GLES2-пути аппаратного декода (сессия 07 плана `user/gles2-renderer/`).
//!
//! Настоящий VA-API decoder thread → DMA-BUF кадр NV12 → адаптер `render-gles2-shell`
//! (`materialize_dma_buf_frame`: кэш импортов → `EGLImage` → две текстуры) → видеослой
//! `gles2-video` → offscreen-цель EGL (по умолчанию 1440×900). Возврат кадра декодеру делает
//! поток-ожидатель адаптера по fence кадра показа (решение владельца 32).
//!
//! Что стенд должен доказать на живом железе:
//! - **нет копии**: `VideoLayerStats.copied_bytes == 0` и `upload_calls == 0`;
//! - **импортов ≈ размер пула**: `imports` кэша не больше числа слотов пула, остальное — попадания;
//! - **пул не истощается**: кадры возвращаются по fence (`released_by_waiter`), а после паузы без
//!   рисования (`--idle-ms`) очередь ожидания пуста (`pending_now == 0`) — «спящий UI»;
//! - **стоимость**: время кадра показа по часам и CPU потока (p50/p95), CPU процесса за окно.
//!
//! Кадры показываются в темпе экрана (`--pace-hz`, 0 — без пауз). `Busy` провайдера ресурсов
//! стенд повторяет на следующем тике (как плеер): прошлый кадр остаётся на экране.
//!
//! Пример: `cargo run --release -p video-vaapi --example gles2_dmabuf_bench -- --input
//! 1080p60_h264.mp4 --layout composed --frames 600 --pace-hz 60`.
//!
//! Модули: `metrics` (CPU, перцентили), `pixel_check` (сверка
//! пикселей), `report` (таблица и `RESULT`).
//!
//! В конце печатается таблица и строка `RESULT {json}` для сбора результатов скриптом.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "стенд замера (пример, не production): отказ окружения или аргументов прерывает \
              прогон с понятным сообщением"
)]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use codec_core::{
    H264Packetization, VideoCodec, VideoRequirementProbe, parse_avc_decoder_configuration_record,
    probe_video_packet_requirement_with_codec_private,
};
use gles2_egui::{FrameStart, Gles2Context, GpuFence, SurfaceSize};
use gles2_video::{VideoLayerOptions, VideoLayerRenderer};
use media_core::{DemuxReadEvent, Demuxer, Packet, TrackInfo, TrackKind};
use render_core::{RenderLiveSettings, RenderViewport};
use render_gles2_shell::{
    DmaBufMaterializeOutcome, DmaBufShownFrom, Gles2FrameReleaseBinding, RendererDetachGrace,
    VideoDrawPlan, VideoPlacement, dma_buf_import_capacity_for, materialize_dma_buf_frame,
    renderable_frame, wrap_video_backend_for_gles2_release,
};
use symphonia_demux::SymphoniaDemuxer;
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProviderHandle,
    VideoBackendDecoderThreadHandle,
};
use video_core::{
    DecodePacket, DecodeSendError, DecodedFrame, FrameResourceDescriptor, VideoDecoderThreadConfig,
    VideoStreamConfigResult, VideoStreamDecodeConfig, VideoStreamPacketization,
};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
use video_vaapi::VaapiVideoBackendFactory;

#[path = "gles2_dmabuf_bench/metrics.rs"]
mod metrics;
#[path = "gles2_dmabuf_bench/pixel_check.rs"]
mod pixel_check;
#[path = "gles2_dmabuf_bench/report.rs"]
mod report;

use metrics::{percentile_ms, process_cpu_time, thread_cpu_time};
use pixel_check::PixelCheck;
use report::RunSummary;

/// Сколько готовых кадров стенд держит у себя (очередь показа плеера): остальные остаются в
/// канале декодера, и пул натурально упирается в backpressure, как в плеере.
const READY_QUEUE_LIMIT: usize = 4;

/// Сколько без новых кадров ждём после конца файла, прежде чем считать поток законченным.
const END_OF_STREAM_QUIET: Duration = Duration::from_secs(2);

/// Параметры прогона из командной строки.
struct BenchArguments {
    input: PathBuf,
    layout: DmaBufImageLayout,
    frame_limit: usize,
    target: [u32; 2],
    pace: Option<Duration>,
    pool_slots: usize,
    gpu_probe_draws: usize,
    idle: Duration,
    /// Сверять пиксели кадров показа (читает цель назад: меняет тайминги, замеры не годятся).
    verify_pixels: bool,
    /// Куда сохранять PPM показанных кадров (для просмотра глазами).
    dump_dir: Option<PathBuf>,
    /// Порядок разборки на выходе (эксперименты по корневой причине падения на T410).
    teardown: TeardownOrder,
}

/// Порядок разборки стенда на выходе.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TeardownOrder {
    /// Как в приложении (сессия 09): рендер уничтожает слой, сообщает привязке об отвязке
    /// (`renderer_detaching`), затем уничтожает контекст. Остальные владельцы (декодер,
    /// провайдер, клоны привязки) падают «как придётся»; ожидания потока по имени нет.
    Natural,
    /// Регрессионный эксперимент: то же, но БЕЗ `renderer_detaching` (рендер «забыл»). Выход
    /// спасает только страховка в `Drop` последнего клона привязки.
    NoDetach,
}

fn parse_arguments() -> BenchArguments {
    let mut input = None;
    let mut layout = DmaBufImageLayout::ComposedLayers;
    let mut frame_limit = 600;
    let mut target = [1440, 900];
    let mut pace_hz: u32 = 60;
    let mut pool_slots = VideoDecoderThreadConfig::default().zero_copy_surface_pool_slots;
    let mut gpu_probe_draws = 0;
    let mut idle_ms: u64 = 500;
    let mut verify_pixels = false;
    let mut dump_dir = None;
    let mut teardown = TeardownOrder::Natural;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .unwrap_or_else(|| panic!("{argument} needs a value"))
        };
        match argument.as_str() {
            "--input" => input = Some(PathBuf::from(value())),
            "--layout" => {
                layout = match value().as_str() {
                    "composed" => DmaBufImageLayout::ComposedLayers,
                    "separate" => DmaBufImageLayout::SeparateLayers,
                    other => panic!("unsupported --layout {other} (composed | separate)"),
                };
            }
            "--frames" => frame_limit = value().parse().expect("--frames is a number"),
            "--target" => {
                let text = value();
                let (width, height) = text.split_once('x').expect("--target WxH");
                target = [
                    width.parse().expect("width"),
                    height.parse().expect("height"),
                ];
            }
            "--pace-hz" => pace_hz = value().parse().expect("--pace-hz is a number"),
            "--pool-slots" => pool_slots = value().parse().expect("--pool-slots is a number"),
            "--gpu-probe" => gpu_probe_draws = value().parse().expect("--gpu-probe is a number"),
            "--idle-ms" => idle_ms = value().parse().expect("--idle-ms is a number"),
            "--verify-pixels" => verify_pixels = true,
            "--dump-dir" => dump_dir = Some(PathBuf::from(value())),
            "--teardown-order" => {
                teardown = match value().as_str() {
                    "natural" => TeardownOrder::Natural,
                    "no-detach" => TeardownOrder::NoDetach,
                    other => panic!("unsupported --teardown-order {other}"),
                }
            }
            other => panic!("unknown argument {other}"),
        }
    }
    BenchArguments {
        input: input.expect("--input PATH"),
        layout,
        frame_limit,
        target,
        pace: (pace_hz > 0).then(|| Duration::from_secs(1) / pace_hz),
        pool_slots,
        gpu_probe_draws,
        idle: Duration::from_millis(idle_ms),
        verify_pixels,
        dump_dir,
        teardown,
    }
}

/// Первая видеодорожка H.264 файла.
fn first_h264_video_track(demuxer: &SymphoniaDemuxer) -> TrackInfo {
    demuxer
        .tracks()
        .iter()
        .find(|track| {
            track.kind == TrackKind::Video
                && VideoCodec::from_container_codec_id(&track.codec_id) == Some(VideoCodec::H264)
        })
        .cloned()
        .expect("the input must contain an H.264 video track")
}

/// Следующий пакет видеодорожки или `None` на конце файла.
fn next_video_packet(demuxer: &mut SymphoniaDemuxer, video_track: &TrackInfo) -> Option<Packet> {
    loop {
        match demuxer.next_event().expect("demux next event") {
            DemuxReadEvent::Packet(packet)
                if packet.kind == TrackKind::Video && packet.track_id == video_track.id =>
            {
                return Some(packet);
            }
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(hint) => {
                panic!("local file unexpectedly reports temporary unavailability: {hint:?}")
            }
            DemuxReadEvent::EndOfStream => return None,
        }
    }
}

/// Конфиг потока как у плеера: требование из первого AU + `avcC`-пакетизация + выбранный layout.
fn h264_stream_config(
    video_track: &TrackInfo,
    first_packet: &Packet,
    layout: DmaBufImageLayout,
) -> VideoStreamDecodeConfig {
    let codec_private = video_track
        .codec_private
        .as_deref()
        .expect("H.264 MP4 must carry avcC");
    let requirement = match probe_video_packet_requirement_with_codec_private(
        VideoCodec::H264,
        &first_packet.data,
        Some(codec_private),
    ) {
        VideoRequirementProbe::Candidate(candidate) => candidate.requirement,
        other => panic!("H.264 requirement probe must succeed, got {other:?}"),
    };
    let record = parse_avc_decoder_configuration_record(codec_private).expect("avcC parses");
    VideoStreamDecodeConfig::from_requirement(
        video_track.id,
        &requirement,
        VideoFrameContract::dma_buf_nv12(layout),
    )
    .with_codec_private(video_track.codec_private.clone())
    .with_packetization(Some(VideoStreamPacketization::H264(
        H264Packetization::from_avc_decoder_configuration_record(&record),
    )))
}

/// Подробности первого дескриптора: модификатор, fourcc, слои, pitch (для отчёта).
fn describe_descriptor(
    provider: &PresentFrameResourceProviderHandle,
    frame: &DecodedFrame,
) -> String {
    match provider.resource_descriptor_lookup(frame.resource_handle) {
        PresentFrameResourceDescriptorLookup::Ready {
            descriptor: FrameResourceDescriptor::DmaBuf(descriptor),
            ..
        } => {
            let modifier = descriptor
                .objects
                .first()
                .map_or(0, |object| object.drm_format_modifier);
            let layer_pitches: Vec<String> = descriptor
                .layers
                .iter()
                .map(|layer| format!("{:?}", &layer.pitch[..layer.num_planes as usize]))
                .collect();
            format!(
                "modifier {modifier:#018x}, fourcc {:#010x}, {}x{}, objects {}, layers {}, pitches {}, export {:?}",
                descriptor.fourcc,
                descriptor.width,
                descriptor.height,
                descriptor.objects.len(),
                descriptor.layers.len(),
                layer_pitches.join(" "),
                descriptor.export_layout,
            )
        }
        other => format!("descriptor unavailable: {other:?}"),
    }
}

/// Исходы показа по видам (то, что печатает отчёт).
#[derive(Default)]
pub struct OutcomeCounters {
    pub cache_hit: u64,
    pub fresh_import: u64,
    pub already_current: u64,
    pub busy: u64,
    pub missing: u64,
    pub provider_fatal: u64,
    pub unsupported: u64,
    pub import_failed: u64,
    pub underruns: u64,
}

/// Экран стенда: контекст, видеослой и всё, что нужно для кадра показа.
struct Gles2Screen {
    context: Gles2Context,
    video: VideoLayerRenderer,
    target: [u32; 2],
    binding: Gles2FrameReleaseBinding,
}

impl Gles2Screen {
    /// Кадр показа с текущим кадром видеослоя; fence отдан слою и привязке освобождения.
    fn draw_current_frame(&mut self, frame: &DecodedFrame) -> Arc<GpuFence> {
        let [width, height] = self.target;
        let plan = VideoDrawPlan::for_frame(
            &renderable_frame(frame).expect("YUV frame"),
            VideoPlacement {
                surface_size: (width, height),
                video_viewport: RenderViewport::full_surface(width, height),
                exclusions: &[],
            },
            &RenderLiveSettings::default(),
        )
        .expect("draw plan");
        match self.context.begin_frame().expect("frame") {
            FrameStart::Ready(active) => {
                self.video.draw(&active, &plan.request()).expect("draw");
                active.present().expect("present");
            }
            FrameStart::Skipped(reason) => panic!("frame skipped: {reason:?}"),
        }
        let fence = Arc::new(self.context.insert_fence().expect("fence"));
        self.video.retire_submitted_work(Arc::clone(&fence));
        self.binding
            .publish_presented_frame_fence(Arc::clone(&fence));
        fence
    }
}

/// Декодер и всё, что нужно для возврата его кадров.
struct DecoderSide {
    /// Обёрнутый декодер: кадры берём отсюда, не дошедшие до экрана возвращаем напрямую.
    decoder: Box<VideoBackendDecoderThreadHandle>,
    /// Обёрнутый провайдер: возврат кадра по fence потоком-ожидателем (как аренда плеера).
    provider: PresentFrameResourceProviderHandle,
    /// Привязка: публикация fence кадра показа и статистика освобождения.
    binding: Gles2FrameReleaseBinding,
}

/// Запускает VA-API, оборачивает его освобождением по fence и настраивает поток.
fn start_decoder(
    arguments: &BenchArguments,
    decoder_config: VideoDecoderThreadConfig,
    video_track: &TrackInfo,
    first_packet: &Packet,
) -> DecoderSide {
    let started = VaapiVideoBackendFactory::new_with_decoder_config(decoder_config)
        .start_for_composition()
        .expect("VA-API backend starts");
    let binding = Gles2FrameReleaseBinding::new();
    let (wrapped, provider) = wrap_video_backend_for_gles2_release(started, &binding);
    let decoder = wrapped.into_decoder_thread();
    assert_eq!(
        decoder.configure_stream(h264_stream_config(
            video_track,
            first_packet,
            arguments.layout
        )),
        VideoStreamConfigResult::Configured,
        "VA-API must accept the stream"
    );
    DecoderSide {
        decoder,
        provider,
        binding,
    }
}

/// Создаёт offscreen-контекст и видеослой с кэшем импортов размером в пул декодера.
fn create_screen(
    arguments: &BenchArguments,
    pool_slots: usize,
    binding: Gles2FrameReleaseBinding,
) -> Gles2Screen {
    let target_size =
        SurfaceSize::new(arguments.target[0], arguments.target[1]).expect("target size");
    let context = Gles2Context::offscreen(target_size).expect("EGL offscreen context");
    println!(
        "GL: {} | {} | layout {:?} | pool slots {pool_slots}",
        context.capabilities().gl_renderer(),
        context.capabilities().gl_version_string(),
        arguments.layout,
    );
    let mut video =
        VideoLayerRenderer::new(&context, VideoLayerOptions::default()).expect("video layer");
    video
        .set_dma_buf_import_capacity(&context, dma_buf_import_capacity_for(pool_slots))
        .expect("import cache capacity");
    Gles2Screen {
        context,
        video,
        target: arguments.target,
        binding,
    }
}

/// Состояние и шаги одного прогона показа.
struct Playback<'a> {
    arguments: &'a BenchArguments,
    decoder: &'a VideoBackendDecoderThreadHandle,
    provider: &'a PresentFrameResourceProviderHandle,
    screen: Gles2Screen,
    demuxer: SymphoniaDemuxer,
    video_track: TrackInfo,
    /// Пакет, который декодер не принял (backpressure) и который надо отправить первым.
    unsent_packet: Option<Packet>,
    input_exhausted: bool,
    /// Кадры декодера, ждущие показа (очередь показа плеера).
    ready: VecDeque<DecodedFrame>,
    /// Кадр, который сейчас на экране.
    on_screen: Option<DecodedFrame>,
    counters: OutcomeCounters,
    pixel_check: PixelCheck,
    show_wall: Vec<Duration>,
    show_cpu: Vec<Duration>,
    descriptor_description: Option<String>,
    first_failure: Option<String>,
    shown: usize,
    last_progress: Instant,
}

impl Playback<'_> {
    /// Отправляет пакеты, пока декодер принимает.
    fn feed_decoder(&mut self) {
        while !self.input_exhausted {
            let Some(packet) = self
                .unsent_packet
                .take()
                .or_else(|| next_video_packet(&mut self.demuxer, &self.video_track))
            else {
                self.input_exhausted = true;
                return;
            };
            let decode_packet = DecodePacket {
                track_id: packet.track_id,
                pts: packet.pts,
                dts: packet.dts,
                track_pts: packet.track_pts,
                track_dts: packet.track_dts,
                generation: 1,
                encoded_bytes: packet.data.clone(),
                keyframe: packet.keyframe.is_known_keyframe(),
                resolved_color: None,
            };
            match self.decoder.send_packet(decode_packet) {
                Ok(()) => {}
                Err(DecodeSendError::Backpressure(_)) => {
                    self.unsent_packet = Some(packet);
                    return;
                }
                Err(DecodeSendError::Fatal(error)) => panic!("decoder: {error}"),
            }
        }
    }

    /// Забирает готовые кадры, пока очередь показа не полна.
    fn collect_frames(&mut self) {
        while self.ready.len() < READY_QUEUE_LIMIT {
            let Some(frame) = self.decoder.try_recv_frame() else {
                break;
            };
            self.descriptor_description
                .get_or_insert_with(|| describe_descriptor(self.provider, &frame));
            self.ready.push_back(frame);
        }
        if let Some(error) = self.decoder.try_recv_error() {
            panic!("decoder error: {error}");
        }
    }

    /// Показывает следующий кадр очереди. `false` — поток закончился.
    fn show_next_frame(&mut self) -> bool {
        let Some(frame) = self.ready.pop_front() else {
            self.counters.underruns += 1;
            if self.input_exhausted && self.last_progress.elapsed() > END_OF_STREAM_QUIET {
                return false;
            }
            std::thread::sleep(Duration::from_micros(200));
            return true;
        };
        let started_wall = Instant::now();
        let started_cpu = thread_cpu_time();
        let materialization = materialize_dma_buf_frame(
            &mut self.screen.video,
            &self.screen.context,
            self.provider,
            &frame,
        );
        match materialization.outcome {
            DmaBufMaterializeOutcome::Shown(from) => {
                self.count_shown(from);
                self.screen.draw_current_frame(&frame);
                if self.arguments.verify_pixels || self.arguments.dump_dir.is_some() {
                    self.pixel_check.observe(
                        &self.screen.context,
                        self.shown,
                        self.arguments.dump_dir.as_deref(),
                    );
                }
                self.show_wall.push(started_wall.elapsed());
                self.show_cpu
                    .push(thread_cpu_time().saturating_sub(started_cpu));
                // Прошлый кадр сменён: возвращаем его как аренду плеера (по fence, потоком-ожидателем).
                if let Some(previous) = self.on_screen.replace(frame) {
                    self.provider.release_frame(previous.resource_handle);
                }
                self.shown += 1;
                self.last_progress = Instant::now();
            }
            DmaBufMaterializeOutcome::Busy => {
                // Как плеер: кадр попробуем на следующем тике, прошлый остаётся на экране.
                self.counters.busy += 1;
                self.ready.push_front(frame);
            }
            outcome => {
                // Кадр не дошёл до экрана: возвращаем декодеру напрямую.
                match &outcome {
                    DmaBufMaterializeOutcome::Missing => self.counters.missing += 1,
                    DmaBufMaterializeOutcome::ProviderFatal => self.counters.provider_fatal += 1,
                    DmaBufMaterializeOutcome::Unsupported(_) => self.counters.unsupported += 1,
                    _ => self.counters.import_failed += 1,
                }
                self.decoder.release_frame(frame.resource_handle);
                self.first_failure = Some(format!("{outcome:?}"));
            }
        }
        true
    }

    fn count_shown(&mut self, from: DmaBufShownFrom) {
        match from {
            DmaBufShownFrom::CacheHit => self.counters.cache_hit += 1,
            DmaBufShownFrom::FreshImport(_) => self.counters.fresh_import += 1,
            DmaBufShownFrom::AlreadyCurrent => self.counters.already_current += 1,
        }
    }

    /// Основной цикл показа в темпе экрана. Возвращает длительность окна замера.
    fn run(&mut self) -> Duration {
        let run_started = Instant::now();
        let mut tick_index = 0u32;
        while self.shown < self.arguments.frame_limit && self.first_failure.is_none() {
            self.feed_decoder();
            self.collect_frames();
            // Темп экрана: кадр не раньше своего тика.
            if let Some(interval) = self.arguments.pace {
                let tick = run_started + interval * tick_index;
                if let Some(wait) = tick.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
            }
            tick_index += 1;
            if !self.show_next_frame() {
                break;
            }
        }
        run_started.elapsed()
    }

    /// Возвращает декодеру последний показанный кадр и всё, что не успели показать.
    fn return_remaining_frames(&mut self) {
        if let Some(last) = self.on_screen.take() {
            self.provider.release_frame(last.resource_handle);
        }
        for frame in self.ready.drain(..) {
            self.decoder.release_frame(frame.resource_handle);
        }
    }
}

fn main() {
    let arguments = parse_arguments();
    let mut demuxer = SymphoniaDemuxer::from_file(&arguments.input)
        .unwrap_or_else(|error| panic!("open {}: {error}", arguments.input.display()));
    let video_track = first_h264_video_track(&demuxer);
    let first_packet = next_video_packet(&mut demuxer, &video_track).expect("first packet");
    let decoder_config = VideoDecoderThreadConfig {
        zero_copy_surface_pool_slots: arguments.pool_slots,
        ..VideoDecoderThreadConfig::from_env()
    }
    .normalized();
    let pool_slots = decoder_config.zero_copy_surface_pool_slots;
    let side = start_decoder(&arguments, decoder_config, &video_track, &first_packet);
    let screen = create_screen(&arguments, pool_slots, side.binding.clone());

    let mut playback = Playback {
        arguments: &arguments,
        decoder: side.decoder.as_ref(),
        provider: &side.provider,
        screen,
        demuxer,
        video_track,
        unsent_packet: Some(first_packet),
        input_exhausted: false,
        ready: VecDeque::new(),
        on_screen: None,
        counters: OutcomeCounters::default(),
        pixel_check: PixelCheck::default(),
        show_wall: Vec::new(),
        show_cpu: Vec::new(),
        descriptor_description: None,
        first_failure: None,
        shown: 0,
        last_progress: Instant::now(),
    };
    let process_cpu_started = process_cpu_time();
    let elapsed = playback.run();
    let process_cpu = process_cpu_time().saturating_sub(process_cpu_started);

    // GPU-проход импортированного NV12 на последнем показанном кадре.
    if arguments.gpu_probe_draws > 0
        && let Some(frame) = playback.on_screen.as_ref()
    {
        run_gpu_probe(&mut playback.screen, frame, arguments.gpu_probe_draws);
    }

    // Конец показа: кадры возвращаются, затем пауза без рисования («спящий UI»).
    playback.return_remaining_frames();
    let pending_right_after_stop = side.binding.stats().pending_now;
    // Остановка декодера до выхода: у потока VA-API нет `Drop`/`join`, и если он ещё декодирует,
    // на выходе процесса он гоняется с atexit-обработчиками драйверов. `flush` возвращается,
    // когда декодер остановил работу.
    side.decoder.flush().expect("flush decoder before shutdown");
    std::thread::sleep(arguments.idle);
    let release_stats = side.binding.stats();
    while let Some(frame) = side.decoder.try_recv_frame() {
        side.decoder.release_frame(frame.resource_handle);
    }

    let mut show_wall_sorted = playback.show_wall.clone();
    show_wall_sorted.sort_unstable();
    let mut show_cpu_sorted = playback.show_cpu.clone();
    show_cpu_sorted.sort_unstable();
    RunSummary {
        layout: arguments.layout,
        pool_slots,
        descriptor_description: playback.descriptor_description.as_deref(),
        shown: playback.shown,
        elapsed,
        process_cpu,
        counters: &playback.counters,
        first_failure: playback.first_failure.as_deref(),
        show_wall_sorted: &show_wall_sorted,
        show_cpu_sorted: &show_cpu_sorted,
        cache: playback.screen.video.dma_buf_import_snapshot(),
        texture_stats: playback.screen.context.dma_buf_texture_stats(),
        release_stats,
        pending_right_after_stop,
        layer_stats: playback.screen.video.stats(),
        pixel_check: arguments.verify_pixels.then_some(&playback.pixel_check),
    }
    .print();

    shutdown(arguments.teardown, playback.screen, side);
}

/// Разборка на выходе. Порядок задаёт `--teardown-order`; цель — показать, что корректность
/// выхода обеспечивает владелец жизненного цикла, а не порядок разборки в стенде.
fn shutdown(order: TeardownOrder, screen: Gles2Screen, side: DecoderSide) {
    let Gles2Screen {
        context,
        video,
        binding: screen_binding,
        ..
    } = screen;
    video.destroy(&context).expect("destroy video layer");
    let DecoderSide {
        decoder,
        provider,
        binding,
    } = side;
    match order {
        TeardownOrder::Natural => {
            // То, что обязан сделать рендер сессии 09: сообщить привязке об отвязке контекста
            // ПЕРЕД его уничтожением. Остальное падает «как придётся» — сначала контекст.
            let detach = screen_binding.renderer_detaching(RendererDetachGrace::default());
            println!("detach: {detach:?}");
            drop(context);
            drop(binding);
            drop(screen_binding);
            drop(provider);
            drop(decoder);
        }
        TeardownOrder::NoDetach => {
            drop(context);
            drop(binding);
            drop(screen_binding);
            drop(provider);
            drop(decoder);
        }
    }
}

/// Время GPU на проход импортированного NV12: N кадров с ожиданием fence каждого (задержка),
/// серия без ожидания (пропускная способность) и серия без кадра видео (только заливка).
fn run_gpu_probe(screen: &mut Gles2Screen, frame: &DecodedFrame, draws: usize) {
    let mut latencies = Vec::with_capacity(draws);
    for _ in 0..draws {
        let started = Instant::now();
        let fence = screen.draw_current_frame(frame);
        fence.wait(Duration::from_secs(5)).expect("probe fence");
        latencies.push(started.elapsed());
    }
    latencies.sort_unstable();
    let series = |screen: &mut Gles2Screen| {
        let started = Instant::now();
        let mut last = None;
        for _ in 0..draws {
            last = Some(screen.draw_current_frame(frame));
        }
        if let Some(fence) = last {
            fence.wait(Duration::from_secs(10)).expect("series fence");
        }
        started.elapsed().as_secs_f64() * 1000.0 / draws as f64
    };
    println!(
        "gpu probe: draw until fence p50 {:.3} ms p95 {:.3} ms",
        percentile_ms(&latencies, 0.5),
        percentile_ms(&latencies, 0.95)
    );
    println!(
        "gpu probe: NV12 series throughput {:.3} ms/frame",
        series(screen)
    );
    screen.video.forget_uploaded_frames();
    println!(
        "gpu probe: clear-only series {:.3} ms/frame",
        series(screen)
    );
}
