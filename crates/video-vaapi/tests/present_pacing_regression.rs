//! Ignored-тест такта декодера по проходам показа (gles2-renderer 12a.2) на настоящем VA-API.
//!
//! Production `VideoDecodeThread` с подпиской на нейтральный `PresentPassSignal`. Тест играет
//! роль плеера и рендера: подаёт пакеты, держит опубликованные кадры (запас «впереди показа»)
//! и сам звонит в сигнал показа. Проверяется результат — темп подтверждения пакетов:
//! 1. запас есть, показов нет — декодер подаёт пакеты не чаще предела ожидания (~100 мс),
//!    а не со скоростью GPU (без такта весь пакетный канал уходит за десятки миллисекунд);
//! 2. запас есть, рендер звонит — пакеты идут в темпе показов, а не предела ожидания;
//! 3. кадры отпущены (запаса нет) — декодер снова работает без такта, со скоростью GPU.
//!
//! Без правки 12a.2 (декодер без такта) падает проверка 1.
//!
//! Запуск: `scripts/media-regression.sh --scenario h264-mp4-vaapi-present-pacing --path <файл>`
//! (H.264 MP4 не короче ~3 с: тесту нужно 40 пакетов).

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

mod support;

use std::thread;
use std::time::{Duration, Instant};

use support::h264_media::{
    decode_packet, first_h264_video_track, h264_stream_config, next_video_packet,
    selected_media_path,
};
use symphonia_demux::SymphoniaDemuxer;
use video_core::{DecodedFrame, PresentPassSignal, VideoStreamConfigResult};
use video_vaapi::{VideoDecodeThread, VideoDecodeThreadConfig};

/// Пакетов в тесте; шлются двумя частями, каждая меньше пакетного канала декодера (32).
const TOTAL_PACKETS: usize = 40;

/// Первая часть: хватает на фазы 1 и 2 (~14 и ~8 подтверждений) с запасом.
const FIRST_BATCH: usize = 30;

/// Пул выходных surfaces с запасом: тест держит до ~25 кадров, и исчерпание пула
/// (другое ожидание декодера) не должно смешиваться с тактом.
const TEST_SURFACE_POOL_FRAMES: usize = 48;

/// Сколько длится фаза «показов нет»: несколько пределов ожидания (production — 100 мс).
const NO_PRESENT_PHASE: Duration = Duration::from_millis(700);

/// Темп показов «рендера» во второй фазе: медленнее декода 1080p на Ironlake (~27 мс)
/// и заметно быстрее предела ожидания.
const PRESENT_PERIOD: Duration = Duration::from_millis(40);

/// Показов во второй фазе.
const PRESENTS: usize = 8;

/// Медианный интервал ACK, ниже которого фаза 1 считается «без такта» (предел — 100 мс).
const PACED_WITHOUT_PRESENT_MIN_INTERVAL: Duration = Duration::from_millis(80);

/// Медианный интервал ACK, выше которого фаза 2 считается «по пределу, а не по показам».
const PACED_BY_PRESENT_MAX_INTERVAL: Duration = Duration::from_millis(75);

/// Пауза опроса подтверждений.
const POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Generation проигрывания с начала файла.
const PLAYBACK_GENERATION: u64 = 1;

#[test]
#[ignore = "requires explicit local H.264 MP4 and VA-API H.264 hardware; use scripts/media-regression.sh"]
fn h264_vaapi_submits_one_picture_per_present_pass_while_frames_are_held() {
    let media_path = selected_media_path();
    let mut demuxer = SymphoniaDemuxer::from_file(&media_path)
        .unwrap_or_else(|error| panic!("open {}: {error}", media_path.display()));
    let video_track = first_h264_video_track(&demuxer);
    let mut packets = Vec::with_capacity(TOTAL_PACKETS);
    while packets.len() < TOTAL_PACKETS {
        packets.push(
            next_video_packet(&mut demuxer, &video_track)
                .expect("the selected file must have at least 40 video packets"),
        );
    }

    let signal = PresentPassSignal::new();
    let config = VideoDecodeThreadConfig {
        decoder_surface_pool_frames: TEST_SURFACE_POOL_FRAMES,
        zero_copy_surface_pool_slots: TEST_SURFACE_POOL_FRAMES,
        ..VideoDecodeThreadConfig::from_env()
    };
    let decoder = VideoDecodeThread::new_with_present_pass_pacing(
        config,
        signal
            .subscribe()
            .expect("fresh signal accepts a subscriber"),
    )
    .expect("VA-API decoder thread should start");
    assert_eq!(
        decoder.configure_stream(h264_stream_config(&video_track, &packets[0])),
        VideoStreamConfigResult::Configured,
        "VA-API must accept the selected H.264 stream"
    );
    let mut run = PacingRun::new(&decoder);
    for packet in &packets[..FIRST_BATCH] {
        decoder
            .send_packet(decode_packet(packet, PLAYBACK_GENERATION))
            .expect("packet channel has room for the first batch");
    }

    // Фаза 1: кадры держим, показов нет — декодер набирает запас и дальше ждёт предела.
    let no_present_acks = run.collect_for(NO_PRESENT_PHASE);
    let no_present_steady = steady_intervals(&no_present_acks);
    let no_present_median = median(&no_present_steady).unwrap_or_else(|| {
        panic!(
            "no ACK interval reached the pacing limit: the decoder is not paced \
             while frames are held (acks at {no_present_acks:?})"
        )
    });
    assert!(
        no_present_median >= PACED_WITHOUT_PRESENT_MIN_INTERVAL,
        "with frames held and no present pass the decoder must wait for the pacing limit, \
         median ACK interval {no_present_median:?} (acks at {no_present_acks:?})"
    );

    // Фаза 2: «рендер» показывает каждые 40 мс — пакеты идут в темпе показов.
    let presenter_signal = signal.clone();
    let presenter = thread::spawn(move || {
        for _ in 0..PRESENTS {
            thread::sleep(PRESENT_PERIOD);
            presenter_signal.notify_present_pass_submitted();
        }
    });
    let present_acks = run.collect_for(PRESENT_PERIOD * u32::try_from(PRESENTS).unwrap());
    presenter.join().expect("presenter thread");
    let present_median = median(&intervals(&present_acks))
        .unwrap_or_else(|| panic!("presents must drive decoding: {present_acks:?}"));
    assert!(
        present_acks.len() <= PRESENTS + 1,
        "at most one paced submission per present pass, got {} ACKs for {PRESENTS} presents",
        present_acks.len()
    );
    assert!(
        present_median <= PACED_BY_PRESENT_MAX_INTERVAL,
        "present passes, not the wait limit, must pace decoding: median {present_median:?}"
    );

    // Фаза 3: кадры отпущены — запаса нет, остаток пакетов идёт без такта.
    run.release_held_frames();
    let remaining = TOTAL_PACKETS - run.acked_packets;
    for packet in &packets[FIRST_BATCH..] {
        decoder
            .send_packet(decode_packet(packet, PLAYBACK_GENERATION))
            .expect("packet channel has room for the second half");
    }
    let unpaced_started = Instant::now();
    run.release_frames_until_acked(TOTAL_PACKETS);
    let unpaced_elapsed = unpaced_started.elapsed();
    // 75 мс на пакет: выше декода 1080p на Ironlake (~30–60 мс), ниже предела ожидания (100 мс).
    assert!(
        unpaced_elapsed
            < PACED_WITHOUT_PRESENT_MIN_INTERVAL * u32::try_from(remaining).unwrap() * 15 / 16,
        "without held frames the decoder must not wait for presents: {remaining} packets took {unpaced_elapsed:?}"
    );
    println!(
        "vaapi_present_pacing no_present_median_ms={:.1} present_median_ms={:.1} present_acks={} unpaced_packets={} unpaced_ms={:.1}",
        no_present_median.as_secs_f64() * 1000.0,
        present_median.as_secs_f64() * 1000.0,
        present_acks.len(),
        remaining,
        unpaced_elapsed.as_secs_f64() * 1000.0,
    );
}

/// Учёт подтверждений и удержание кадров одной generation.
struct PacingRun<'decoder> {
    decoder: &'decoder VideoDecodeThread,
    /// Кадры, которые «рендер» ещё держит (запас впереди показа).
    held_frames: Vec<DecodedFrame>,
    /// Подтверждено пакетов за весь тест.
    acked_packets: usize,
}

impl<'decoder> PacingRun<'decoder> {
    fn new(decoder: &'decoder VideoDecodeThread) -> Self {
        Self {
            decoder,
            held_frames: Vec::new(),
            acked_packets: 0,
        }
    }

    /// Держит кадры и записывает момент каждого подтверждения в течение `phase`.
    fn collect_for(&mut self, phase: Duration) -> Vec<Duration> {
        let started = Instant::now();
        let mut ack_moments = Vec::new();
        while started.elapsed() < phase {
            let acknowledged = self.decoder.drain_completed_packet_count();
            for _ in 0..acknowledged {
                ack_moments.push(started.elapsed());
            }
            self.acked_packets += acknowledged;
            self.hold_published_frames();
            self.fail_on_decoder_error();
            thread::sleep(POLL_INTERVAL);
        }
        ack_moments
    }

    /// Отпускает кадры по мере публикации, пока не подтверждены `total` пакетов.
    fn release_frames_until_acked(&mut self, total: usize) {
        let started = Instant::now();
        while self.acked_packets < total {
            self.acked_packets += self.decoder.drain_completed_packet_count();
            self.hold_published_frames();
            self.release_held_frames();
            self.fail_on_decoder_error();
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "decoder stalled"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn hold_published_frames(&mut self) {
        while let Some(frame) = self.decoder.try_recv_frame() {
            assert_eq!(
                frame.generation, PLAYBACK_GENERATION,
                "frame of an unexpected generation"
            );
            self.held_frames.push(frame);
        }
    }

    fn release_held_frames(&mut self) {
        for frame in self.held_frames.drain(..) {
            self.decoder.release_frame(frame.resource_handle);
        }
    }

    fn fail_on_decoder_error(&self) {
        if let Some(error) = self.decoder.try_recv_error() {
            panic!("VA-API decoder thread ended with an error: {error}");
        }
    }
}

/// Интервалы между соседними подтверждениями.
fn intervals(moments: &[Duration]) -> Vec<Duration> {
    moments.windows(2).map(|pair| pair[1] - pair[0]).collect()
}

/// Интервалы после набора запаса: первые подтверждения идут без такта (запас < порога) со
/// скоростью GPU — на Ironlake это 30–60 мс на картинку 1080p. Хвост начинается с первого
/// интервала, дотянувшего до предела ожидания: раньше него такт ещё не включился.
fn steady_intervals(moments: &[Duration]) -> Vec<Duration> {
    let all = intervals(moments);
    let first_paced = all
        .iter()
        .position(|interval| *interval >= PACED_WITHOUT_PRESENT_MIN_INTERVAL)
        .unwrap_or(all.len());
    all[first_paced..].to_vec()
}

fn median(values: &[Duration]) -> Option<Duration> {
    let mut sorted = values.to_vec();
    sorted.sort();
    sorted.get(sorted.len() / 2).copied()
}
