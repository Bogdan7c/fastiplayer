//! Стенд замера GLES2-пути программного декода (сессия 05 плана `user/gles2-renderer/`).
//!
//! Настоящий FFmpeg decoder thread → дескриптор кадра → адаптер `render-gles2-shell` →
//! видеослой `gles2-video` → offscreen-цель EGL (по умолчанию 1440×900, как панель T410). Меряет
//! на главном потоке:
//! - `upload` — загрузка кадра (`VideoLayerRenderer::upload`: одна копия драйвером), время по
//!   часам и CPU-время потока (`/proc/thread-self/schedstat`): если они равны, копию делает
//!   процессор в нашем потоке, если CPU намного меньше — поток ждёт GPU (blit/синхронизация);
//! - `draw` — кадр показа целиком (`begin_frame` → видео → `present` → fence).
//!
//! Кадры показываются в темпе экрана (`--pace-hz`, по умолчанию 60; 0 — без пауз). Если кольцо
//! ответило `Busy` (GPU ещё читает слоты), стенд с темпом делает то же, что плеер: показывает
//! прошлый кадр и пробует тот же кадр на следующем тике; без темпа — просто ждёт слот. Повторы
//! считаются отдельно. Если декодер медленнее темпа, отстающий стенд показывает кадры пачками,
//! и `Busy` — честный ответ кольца из 3 слотов, а не дефект: плеер показывает кадры по PTS.
//!
//! `--gpu-probe N` — время GPU на проход видео: N кадров с ожиданием fence каждого, серия из N
//! кадров без ожидания (пропускная способность) и серия без кадра видео (только заливка).
//!
//! Кадры возвращаются декодеру по fence их кадра показа (решение владельца сессии 05).
//!
//! Вход — сырые потоки без контейнера (контейнер разбирает плеер, а не стенд):
//! - `--codec h264 --input file.h264` — Annex B с AUD (`ffmpeg … -bsf:v
//!   h264_mp4toannexb,h264_metadata=aud=insert -f h264`);
//! - `--codec vp9 --input file.ivf` — IVF (`ffmpeg … -c copy -f ivf`);
//! - `--codec hevc --input file.h265` — Annex B с AUD (`ffmpeg … -c copy -bsf:v
//!   hevc_mp4toannexb,hevc_metadata=aud=insert -f hevc`).
//!
//! `--ten-bit` — кадры 4:2:0 10 бит (`yuv420p10le`, сессия 06): цвет кадра (в том числе HDR PQ/HLG)
//! FFmpeg берёт из битового потока, HDR → SDR — по настройкам по умолчанию.
//!
//! Пример: `cargo run --release -p video-ffmpeg --features vendored-ffmpeg --example
//! gles2_upload_bench -- --codec vp9 --input vp9-4k60.ivf --frames 600`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "стенд замера (пример, не production): отказ окружения или аргументов прерывает \
              прогон с понятным сообщением"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::{BitDepth, VideoCodec, VideoDecodeRequirement};
use gles2_egui::{FenceStatus, FrameStart, Gles2Context, GpuFence, OffscreenAlpha, SurfaceSize};
use gles2_video::{
    PlaneFormatPreference, RowLengthPreference, UploadOutcome, VideoLayerOptions,
    VideoLayerRenderer,
};
use media_core::TrackId;
use render_core::{RenderLiveSettings, RenderViewport};
use render_gles2_shell::{
    VideoDrawPlan, VideoPlacement, host_planar_frame_upload, renderable_frame,
};
use video_backend_api::{PresentFrameResourceDescriptorLookup, VideoBackendDecoderThreadHandle};
use video_core::{
    DecodePacket, DecodeSendError, DecodedFrame, FrameResourceHandle, VideoStreamConfigResult,
    VideoStreamDecodeConfig,
};
use video_ffmpeg::FfmpegSoftwareVideoBackendFactory;
use video_frame_contract::VideoFrameContract;

const TRACK_ID: TrackId = TrackId::new(1);

/// Параметры прогона из командной строки.
struct BenchArguments {
    codec: VideoCodec,
    input: PathBuf,
    frame_limit: usize,
    target: [u32; 2],
    row_length: RowLengthPreference,
    pace: Option<Duration>,
    gpu_probe_draws: usize,
    plane_format: PlaneFormatPreference,
    ten_bit: bool,
}

fn parse_arguments() -> BenchArguments {
    let mut codec = None;
    let mut input = None;
    let mut frame_limit = 600;
    let mut target = [1440, 900];
    let mut row_length = RowLengthPreference::BestAvailable;
    let mut pace_hz: u32 = 60;
    let mut gpu_probe_draws = 0;
    let mut plane_format = PlaneFormatPreference::BestAvailable;
    let mut ten_bit = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .unwrap_or_else(|| panic!("{argument} needs a value"))
        };
        match argument.as_str() {
            "--codec" => {
                codec = Some(match value().as_str() {
                    "h264" => VideoCodec::H264,
                    "vp9" => VideoCodec::Vp9,
                    "hevc" => VideoCodec::H265,
                    other => panic!("unsupported --codec {other} (h264 | vp9 | hevc)"),
                });
            }
            "--input" => input = Some(PathBuf::from(value())),
            "--frames" => frame_limit = value().parse().expect("--frames is a number"),
            "--target" => {
                let text = value();
                let (width, height) = text.split_once('x').expect("--target WxH");
                target = [
                    width.parse().expect("width"),
                    height.parse().expect("height"),
                ];
            }
            "--stride-wide" => row_length = RowLengthPreference::StrideWideTextures,
            "--pace-hz" => pace_hz = value().parse().expect("--pace-hz is a number"),
            "--luminance" => plane_format = PlaneFormatPreference::LuminanceOnly,
            "--gpu-probe" => gpu_probe_draws = value().parse().expect("--gpu-probe is a number"),
            "--ten-bit" => ten_bit = true,
            other => panic!("unknown argument {other}"),
        }
    }
    BenchArguments {
        codec: codec.expect("--codec h264|vp9|hevc"),
        input: input.expect("--input PATH"),
        frame_limit,
        target,
        row_length,
        pace: (pace_hz > 0).then(|| Duration::from_secs(1) / pace_hz),
        gpu_probe_draws,
        plane_format,
        ten_bit,
    }
}

/// CPU-время текущего потока (первое поле `/proc/thread-self/schedstat`, наносекунды).
fn thread_cpu_time() -> Duration {
    let schedstat = std::fs::read_to_string("/proc/thread-self/schedstat").expect("schedstat");
    let nanos = schedstat
        .split_whitespace()
        .next()
        .and_then(|field| field.parse().ok())
        .expect("schedstat cpu time");
    Duration::from_nanos(nanos)
}

/// Пакеты IVF (32 байта заголовка файла, 12 — кадра).
fn ivf_packets(file_bytes: &[u8]) -> Vec<Bytes> {
    assert_eq!(&file_bytes[0..4], b"DKIF", "IVF signature");
    let mut packets = Vec::new();
    let mut offset = 32;
    while offset + 12 <= file_bytes.len() {
        let frame_len =
            u32::from_le_bytes(file_bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let start = offset + 12;
        packets.push(Bytes::copy_from_slice(
            &file_bytes[start..start + frame_len],
        ));
        offset = start + frame_len;
    }
    packets
}

/// Тип NAL разделителя access unit-ов (AUD): H.264 — 9 (младшие 5 бит), HEVC — 35 (биты 1–6).
fn is_access_unit_delimiter(codec: VideoCodec, nal_header: u8) -> bool {
    match codec {
        VideoCodec::H265 => (nal_header >> 1) & 0x3F == 35,
        _ => nal_header & 0x1F == 9,
    }
}

/// Access unit-ы Annex B по разделителям AUD (`00 00 01` + AUD после любого числа нулей).
fn annex_b_access_units(codec: VideoCodec, file_bytes: &[u8]) -> Vec<Bytes> {
    let mut starts = Vec::new();
    for index in 0..file_bytes.len().saturating_sub(3) {
        if file_bytes[index..index + 3] == [0, 0, 1]
            && is_access_unit_delimiter(codec, file_bytes[index + 3])
        {
            // Включаем ведущий ноль 4-байтного стартового кода.
            let start = if index > 0 && file_bytes[index - 1] == 0 {
                index - 1
            } else {
                index
            };
            starts.push(start);
        }
    }
    starts.push(file_bytes.len());
    starts
        .windows(2)
        .map(|bounds| Bytes::copy_from_slice(&file_bytes[bounds[0]..bounds[1]]))
        .collect()
}

/// Сводка выборки длительностей.
fn summary(name: &str, samples: &mut [Duration]) -> String {
    samples.sort_unstable();
    let millis = |duration: Duration| duration.as_secs_f64() * 1000.0;
    let percentile = |fraction: f64| {
        let index = ((samples.len() as f64 - 1.0) * fraction).round() as usize;
        millis(samples[index])
    };
    let mean = samples.iter().map(|sample| millis(*sample)).sum::<f64>() / samples.len() as f64;
    format!(
        "{name}: mean {mean:.3} ms, p50 {:.3}, p90 {:.3}, p99 {:.3}, max {:.3}",
        percentile(0.5),
        percentile(0.9),
        percentile(0.99),
        percentile(1.0)
    )
}

/// Кадр декодера, ожидающий fence своего кадра показа.
struct InFlightFrame {
    fence: Arc<GpuFence>,
    handle: FrameResourceHandle,
}

fn release_completed(
    decoder: &VideoBackendDecoderThreadHandle,
    in_flight: &mut Vec<InFlightFrame>,
) {
    in_flight.retain(|frame| {
        let done = frame.fence.poll().expect("fence poll") == FenceStatus::Signaled;
        if done {
            decoder.release_frame(frame.handle);
        }
        !done
    });
}

/// Главный поток: загрузка и показ кадров, учёт времени.
struct Gles2Screen {
    context: Gles2Context,
    video: VideoLayerRenderer,
    target: [u32; 2],
    upload_times: Vec<Duration>,
    upload_cpu_times: Vec<Duration>,
    draw_times: Vec<Duration>,
    busy_frames: usize,
}

impl Gles2Screen {
    fn present(
        &mut self,
        decoder: &VideoBackendDecoderThreadHandle,
        frame: &DecodedFrame,
        in_flight: &mut Vec<InFlightFrame>,
        pace: Option<Duration>,
    ) {
        let provider = decoder.resource_provider();
        let PresentFrameResourceDescriptorLookup::Ready { descriptor, .. } =
            provider.resource_descriptor_lookup(frame.resource_handle)
        else {
            panic!("decoded frame has no descriptor");
        };
        let upload = host_planar_frame_upload(frame, &descriptor).expect("adapter maps the frame");
        loop {
            let upload_started = Instant::now();
            let upload_cpu_started = thread_cpu_time();
            let outcome = self.video.upload(&self.context, &upload).expect("upload");
            let upload_cpu = thread_cpu_time().saturating_sub(upload_cpu_started);
            let upload_wall = upload_started.elapsed();
            match outcome {
                UploadOutcome::Uploaded(_) => {
                    self.upload_times.push(upload_wall);
                    self.upload_cpu_times.push(upload_cpu);
                    break;
                }
                UploadOutcome::AlreadyCurrent => break,
                UploadOutcome::Busy(_) => {
                    self.busy_frames += 1;
                    match pace {
                        // Как плеер: прошлый кадр остаётся на экране, тот же кадр — на следующем
                        // тике экрана.
                        Some(interval) => {
                            self.draw_current_frame(frame);
                            std::thread::sleep(interval);
                        }
                        // Без темпа показывать нечего: ждём, пока GPU освободит слот.
                        None => std::thread::sleep(Duration::from_millis(1)),
                    }
                }
            }
        }

        let fence = self.draw_current_frame(frame);
        // Память кадра держит декодер, пока ему не вернут кадр: возвращаем только по fence.
        in_flight.push(InFlightFrame {
            fence,
            handle: frame.resource_handle,
        });
    }

    /// Кадр показа с текущим кадром видеослоя; fence отдан видеослою и возвращается вызывающему.
    fn draw_current_frame(&mut self, frame: &DecodedFrame) -> Arc<GpuFence> {
        let draw_started = Instant::now();
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
        self.draw_times.push(draw_started.elapsed());
        fence
    }
}

fn main() {
    let arguments = parse_arguments();
    let file_bytes = std::fs::read(&arguments.input).expect("read input stream");
    let packets = match arguments.codec {
        VideoCodec::Vp9 => ivf_packets(&file_bytes),
        codec => annex_b_access_units(codec, &file_bytes),
    };
    let started = FfmpegSoftwareVideoBackendFactory::new()
        .start_for_composition()
        .expect("software FFmpeg backend starts");
    let decoder = started.into_decoder_thread();
    let (requirement, output_contract) = if arguments.ten_bit {
        (
            VideoDecodeRequirement::new(arguments.codec).with_bit_depth(BitDepth::Ten),
            VideoFrameContract::host_yuv420_planar10le(),
        )
    } else {
        (
            VideoDecodeRequirement::new(arguments.codec),
            VideoFrameContract::host_yuv420_planar8(),
        )
    };
    let config = VideoStreamDecodeConfig::from_requirement(TRACK_ID, &requirement, output_contract);
    assert_eq!(
        decoder.configure_stream(config),
        VideoStreamConfigResult::Configured
    );

    let target_size =
        SurfaceSize::new(arguments.target[0], arguments.target[1]).expect("target size");
    let context = Gles2Context::offscreen_with_alpha(target_size, OffscreenAlpha::Opaque)
        .expect("EGL offscreen context");
    let capabilities = context.capabilities();
    println!(
        "GL: {} | {} | unpack_subimage {} | texture_rg {}",
        capabilities.gl_renderer(),
        capabilities.gl_version_string(),
        capabilities.tiers().conveniences.unpack_subimage,
        capabilities.gl_extensions().contains("GL_EXT_texture_rg"),
    );
    let video = VideoLayerRenderer::new(
        &context,
        VideoLayerOptions {
            row_length: arguments.row_length,
            plane_format: arguments.plane_format,
            ..VideoLayerOptions::default()
        },
    )
    .expect("video layer");
    let mut screen = Gles2Screen {
        context,
        video,
        target: arguments.target,
        upload_times: Vec::new(),
        upload_cpu_times: Vec::new(),
        draw_times: Vec::new(),
        busy_frames: 0,
    };
    let mut in_flight = Vec::new();
    let mut presented = 0;
    let run_started = Instant::now();
    let mut packets = packets.into_iter().enumerate().peekable();
    while presented < arguments.frame_limit {
        if let Some((index, payload)) = packets.peek() {
            let packet = DecodePacket {
                track_id: TRACK_ID,
                pts: Duration::from_millis(16) * u32::try_from(*index).unwrap(),
                dts: None,
                track_pts: None,
                track_dts: None,
                generation: 1,
                encoded_bytes: payload.clone(),
                keyframe: *index == 0,
                resolved_color: None,
            };
            match decoder.send_packet(packet) {
                Ok(()) => {
                    packets.next();
                }
                Err(DecodeSendError::Backpressure(_)) => {}
                Err(DecodeSendError::Fatal(error)) => panic!("decoder: {error}"),
            }
        } else if in_flight.is_empty() && decoder.try_recv_frame().is_none() {
            break;
        }
        while let Some(frame) = decoder.try_recv_frame() {
            if let Some(interval) = arguments.pace {
                // Темп экрана: следующий кадр не раньше своего тика.
                let tick = run_started + interval * u32::try_from(presented).unwrap();
                if let Some(wait) = tick.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
            }
            screen.present(decoder.as_ref(), &frame, &mut in_flight, arguments.pace);
            presented += 1;
            if arguments.gpu_probe_draws > 0 {
                // Время GPU на проход видео: каждый кадр показа ждёт свой fence (задержка
                // кадра), затем серия без ожидания с одним fence в конце (пропускная способность,
                // как стенд сессии 00), затем та же серия без кадра видео (только заливка цели).
                let mut probe_times = Vec::with_capacity(arguments.gpu_probe_draws);
                for _ in 0..arguments.gpu_probe_draws {
                    let started = Instant::now();
                    let fence = screen.draw_current_frame(&frame);
                    fence.wait(Duration::from_secs(5)).expect("probe fence");
                    probe_times.push(started.elapsed());
                }
                let series = |screen: &mut Gles2Screen| {
                    let started = Instant::now();
                    let mut last = None;
                    for _ in 0..arguments.gpu_probe_draws {
                        last = Some(screen.draw_current_frame(&frame));
                    }
                    if let Some(fence) = last {
                        fence.wait(Duration::from_secs(10)).expect("series fence");
                    }
                    started.elapsed().as_secs_f64() * 1000.0 / arguments.gpu_probe_draws as f64
                };
                println!(
                    "gpu probe: series throughput {:.3} ms/frame",
                    series(&mut screen)
                );
                screen.video.forget_uploaded_frames();
                println!(
                    "gpu probe: clear-only series {:.3} ms/frame",
                    series(&mut screen)
                );
                println!(
                    "{}",
                    summary("gpu probe: draw until fence", &mut probe_times)
                );
                in_flight.iter().for_each(|frame| {
                    frame.fence.wait(Duration::from_secs(5)).expect("fence");
                });
                release_completed(decoder.as_ref(), &mut in_flight);
                screen
                    .video
                    .destroy(&screen.context)
                    .expect("destroy video layer");
                return;
            }
            release_completed(decoder.as_ref(), &mut in_flight);
        }
        release_completed(decoder.as_ref(), &mut in_flight);
        if let Some(error) = decoder.try_recv_error() {
            panic!("decoder error: {error}");
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    let elapsed = run_started.elapsed();
    let stats = screen.video.stats();
    println!(
        "{presented} frames in {:.2} s ({:.1} fps), uploaded {}, busy retries {}, upload calls {}, copied {:.1} MiB/frame",
        elapsed.as_secs_f64(),
        presented as f64 / elapsed.as_secs_f64(),
        stats.frames_uploaded,
        screen.busy_frames,
        stats.upload_calls,
        stats.copied_bytes as f64 / stats.frames_uploaded.max(1) as f64 / (1024.0 * 1024.0),
    );
    println!("{}", summary("upload wall", &mut screen.upload_times));
    println!(
        "{}",
        summary("upload thread CPU", &mut screen.upload_cpu_times)
    );
    println!("{}", summary("draw+present+fence", &mut screen.draw_times));
    println!("HDR tone-mapping tables built: {}", stats.hdr_table_builds);
    for frame in &in_flight {
        frame
            .fence
            .wait(Duration::from_secs(5))
            .expect("final fence");
    }
    release_completed(decoder.as_ref(), &mut in_flight);
    screen
        .video
        .destroy(&screen.context)
        .expect("destroy video layer");
}
