//! Звук после drag-а по таймлайну: forward extension живого прохода не теряет PCM цели.
//!
//! Дефект D2 (живая приёмка, MP4): после отпускания мыши звук опережал картинку на
//! 0,3–2,6 с до следующей перемотки. Проверяется то, что услышит зритель: PCM, записанный
//! в output после последней очистки ring-а (очистка = начало нового отрезка). Каждый
//! sample тестового звука равен своему media-времени в миллисекундах, поэтому по ring-у
//! видно, какой момент файла прозвучит в точке привязки часов.

use std::sync::Arc;

use audio_core::{AudioChannelLayout, AudioDecoder, EncodedAudioPacket};

use super::test_support::*;
use super::*;
use crate::{
    AudioOutputDeviceRequest, AudioOutputDeviceRoute, AudioOutputInputFrameCount,
    AudioOutputStreamFrameCount, AudioOutputWriteError, AudioOutputWriteIntent,
    AudioOutputWriteReport, CreatedAudioOutput,
};

/// Частота PCM тестового звука.
const SAMPLE_RATE: u32 = 48_000;

/// Каналов в тестовом PCM.
const CHANNELS: usize = 2;

/// Длительность одного audio packet-а (как AAC/Opus порядка 20 мс).
const AUDIO_PACKET_DURATION: Duration = Duration::from_millis(20);

/// Шаг видеокадров тестового файла (25 fps).
const VIDEO_FRAME_STEP: Duration = Duration::from_millis(40);

/// Видеодорожка.
const VIDEO_TRACK: TrackId = TrackId::new(1);

/// Аудиодорожка.
const AUDIO_TRACK: TrackId = TrackId::new(2);

/// Допуск A/V, который зритель не замечает (как в e2e «до кадра»).
const AV_TOLERANCE_MS: f32 = 40.0;

/// PCM frames за одну миллисекунду.
const FRAMES_PER_MS: f32 = SAMPLE_RATE as f32 / 1_000.0;

/// Decoder, который кладёт в каждый PCM sample его media-время в миллисекундах.
///
/// PTS packet-а (мс) приходит в payload-е: так decoder не зависит от container timing.
struct MediaTimeToneDecoder;

impl AudioDecoder for MediaTimeToneDecoder {
    /// Отдаёт 20 мс stereo PCM: sample = PTS packet-а + смещение кадра внутри packet-а.
    fn decode(&mut self, packet: &EncodedAudioPacket<'_>) -> anyhow::Result<Vec<f32>> {
        let pts_bytes: [u8; 4] = packet
            .data()
            .try_into()
            .map_err(|_| anyhow::anyhow!("test audio payload must carry u32 pts"))?;
        let packet_start_ms = u32::from_le_bytes(pts_bytes) as f32;
        let packet_frames = frames_in(AUDIO_PACKET_DURATION);
        let mut samples = Vec::with_capacity(packet_frames * CHANNELS);
        for frame_index in 0..packet_frames {
            let media_time_ms = packet_start_ms + frame_index as f32 / FRAMES_PER_MS;
            samples.extend(std::iter::repeat_n(media_time_ms, CHANNELS));
        }
        Ok(samples)
    }

    /// Stateless fake: сбрасывать нечего.
    fn reset(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn channels(&self) -> u32 {
        CHANNELS as u32
    }

    fn channel_layout(&self) -> Option<AudioChannelLayout> {
        Some(AudioChannelLayout::stereo())
    }
}

/// Журнал output-а: PCM каждого отрезка между очистками ring-а.
#[derive(Default)]
struct SegmentLog {
    /// Отрезки в порядке записи; очистка под seek открывает новый.
    segments: Vec<Vec<f32>>,
}

/// Output, который помнит, какой PCM лежит в ring-е после последней очистки.
struct SegmentRecordingOutput {
    /// Общий с тестом журнал.
    log: Arc<Mutex<SegmentLog>>,

    /// Часы output-а (позицию тест не двигает: проверяется содержимое ring-а).
    clock: Arc<ScriptedAudioClock>,
}

impl SegmentRecordingOutput {
    /// PCM frames текущего отрезка.
    fn current_segment_frames(&self) -> usize {
        let log = self.log.lock().expect("segment log lock");
        log.segments
            .last()
            .map_or(0, |segment| segment.len() / CHANNELS)
    }
}

impl PlayerAudioOutput for SegmentRecordingOutput {
    fn write_samples(
        &mut self,
        samples: &[f32],
        _intent: AudioOutputWriteIntent,
    ) -> std::result::Result<AudioOutputWriteReport, AudioOutputWriteError> {
        let mut log = self.log.lock().expect("segment log lock");
        if log.segments.is_empty() {
            log.segments.push(Vec::new());
        }
        log.segments
            .last_mut()
            .expect("segment exists")
            .extend_from_slice(samples);
        let frames = samples.len() / CHANNELS;
        Ok(AudioOutputWriteReport::complete(
            AudioOutputInputFrameCount::new(frames),
            AudioOutputStreamFrameCount::new(frames),
        ))
    }

    fn play(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<audio_core::AudioOutputClockTiming> {
        Ok(self.clock.output_timing())
    }

    /// Очистка под seek: следующий PCM ляжет в точку привязки часов.
    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        self.log
            .lock()
            .expect("segment log lock")
            .segments
            .push(Vec::new());
        Ok(generation)
    }

    fn set_volume(&mut self, _volume: f32) {}

    /// Уровень ring-а = записанный PCM текущего отрезка (воспроизведение стоит).
    fn buffer_level_ms(&self) -> f64 {
        self.current_segment_frames() as f64 / f64::from(FRAMES_PER_MS)
    }

    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        self.clock.as_player_clock()
    }
}

/// Factory, отдающая один записывающий output.
struct SegmentRecordingOutputFactory {
    /// Общий с тестом журнал.
    log: Arc<Mutex<SegmentLog>>,
}

impl AudioOutputFactory for SegmentRecordingOutputFactory {
    fn create_output(
        &self,
        _spec: AudioOutputSpec,
        _device_request: AudioOutputDeviceRequest,
    ) -> anyhow::Result<CreatedAudioOutput> {
        let output = SegmentRecordingOutput {
            log: self.log.clone(),
            clock: Arc::new(ScriptedAudioClock::new()),
        };
        Ok(CreatedAudioOutput::new(
            Box::new(output),
            AudioOutputDeviceRoute::SelectedDevice,
        ))
    }
}

/// PCM frames в длительности.
fn frames_in(duration: Duration) -> usize {
    usize::try_from(duration.as_nanos() * u128::from(SAMPLE_RATE) / 1_000_000_000)
        .expect("test durations fit usize")
}

/// Audio packet, payload которого несёт PTS в миллисекундах.
fn media_time_audio_packet(pts: Duration) -> media_core::Packet {
    let pts_ms = u32::try_from(pts.as_millis()).expect("test pts fits u32");
    media_core::Packet::new_unbounded(
        AUDIO_TRACK,
        TrackKind::Audio,
        pts,
        None,
        true,
        Bytes::copy_from_slice(&pts_ms.to_le_bytes()),
    )
    .with_duration(AUDIO_PACKET_DURATION)
}

/// Перемешанные (как в MP4/MKV) video и audio packets от keyframe до `end`.
fn interleaved_packets(keyframe: Duration, end: Duration) -> Vec<media_core::Packet> {
    let mut packets = Vec::new();
    let mut video_pts = keyframe;
    let mut audio_pts = keyframe;
    while video_pts < end || audio_pts < end {
        if video_pts <= audio_pts && video_pts < end {
            let keyframe_kind = if video_pts == keyframe {
                PacketKeyframe::Keyframe
            } else {
                PacketKeyframe::NotKeyframe
            };
            packets.push(fake_video_packet_with_keyframe(
                VIDEO_TRACK,
                video_pts,
                keyframe_kind,
            ));
            video_pts += VIDEO_FRAME_STEP;
        } else {
            packets.push(media_time_audio_packet(audio_pts));
            audio_pts += AUDIO_PACKET_DURATION;
        }
    }
    packets
}

/// A/V harness, у которого звук реально декодируется и пишется в записывающий output.
fn av_scrub_harness(
    first_target: Duration,
    keyframe: Duration,
    read_end: Duration,
) -> (SeekRegressionHarness, Arc<Mutex<SegmentLog>>) {
    let tracks = vec![
        fake_track(VIDEO_TRACK.get(), TrackKind::Video),
        fake_track(AUDIO_TRACK.get(), TrackKind::Audio),
    ];
    let demuxer = scripted_seek_demuxer(
        tracks.clone(),
        first_target,
        keyframe,
        interleaved_packets(keyframe, read_end),
    );
    let mut harness = SeekRegressionHarness::new(tracks, demuxer);
    let log = Arc::new(Mutex::new(SegmentLog::default()));
    harness.session.audio_output_factory =
        Arc::new(SegmentRecordingOutputFactory { log: log.clone() });
    harness
        .session
        .pipeline
        .install_audio_decoder(Box::new(MediaTimeToneDecoder));
    (harness, log)
}

/// PCM, который лежит в ring-е после последней очистки (его и услышит зритель).
fn audible_segment(log: &Arc<Mutex<SegmentLog>>) -> Vec<f32> {
    log.lock()
        .expect("segment log lock")
        .segments
        .last()
        .cloned()
        .unwrap_or_default()
}

/// Media-время (мс) каждого PCM frame-а отрезка; тишина = 0.
fn frame_media_times_ms(segment: &[f32]) -> Vec<f32> {
    let (frames, _) = segment.as_chunks::<CHANNELS>();
    frames.iter().map(|frame| frame[0]).collect()
}

/// Начинает drag и показывает первый preview до landing-кадра.
fn begin_drag_with_landed_preview(harness: &mut SeekRegressionHarness, first_target: Duration) {
    harness
        .session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    harness
        .session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_duration(first_target),
        )))
        .unwrap();
    for _ in 0..6 {
        let _ = harness.tick_once_fast_preroll();
    }
    harness.push_decoded_frame(first_target, 80, 1);
    let _ = harness.tick_once_fast_preroll();
}

/// Двигает drag вперёд в пределах капа extension-а и показывает кадр новой цели.
fn extend_drag_forward_to(harness: &mut SeekRegressionHarness, extended_target: Duration) {
    harness
        .session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_duration(extended_target),
        )))
        .unwrap();
    harness.push_decoded_frame(extended_target, 81, 1);
    for _ in 0..4 {
        let _ = harness.tick_once_fast_preroll();
    }
}

/// Отпускает мышь на видимом кадре и доводит commit до конца.
fn release_on_visible_frame(harness: &mut SeekRegressionHarness) {
    harness
        .session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitVisiblePreview,
        ))
        .unwrap();
    for _ in 0..12 {
        if harness.session.seek_commit().is_none() {
            break;
        }
        let _ = harness.tick_once_fast_preroll();
    }
}

/// Проверяет, что ring начинается звуком цели и идёт без дыр (то, что услышит зритель).
fn assert_audio_starts_at_target_without_hole(log: &Arc<Mutex<SegmentLog>>, target: Duration) {
    let media_times_ms = frame_media_times_ms(&audible_segment(log));
    let checked_frames = frames_in(Duration::from_millis(100));
    assert!(
        media_times_ms.len() >= checked_frames,
        "at least 100 ms of audio must be ready after release, got {} frames",
        media_times_ms.len()
    );
    let target_ms = target.as_millis() as f32;
    for (frame_index, heard_ms) in media_times_ms.iter().take(checked_frames).enumerate() {
        // Frame номер N звучит через N/48 мс после точки привязки часов (= цели).
        let expected_ms = target_ms + frame_index as f32 / FRAMES_PER_MS;
        assert!(
            (heard_ms - expected_ms).abs() <= AV_TOLERANCE_MS,
            "frame {frame_index} after release plays media {heard_ms} ms, expected \
             {expected_ms} ms (0 = silence hole)"
        );
    }
}

#[test]
fn drag_forward_extension_then_release_plays_audio_of_the_released_frame() {
    let keyframe = Duration::from_millis(7_800);
    let first_target = Duration::from_secs(8);
    // Шаг drag-а меньше, чем звука было бы декодировано под первую цель (200 мс
    // high-water): именно этот PCM extension раньше выбрасывал вместе с очисткой output-а.
    let extended_target = Duration::from_millis(8_120);
    // Demux успевает прочитать далеко за цель, как в MP4 с глубоким read-ahead.
    let (mut harness, log) =
        av_scrub_harness(first_target, keyframe, Duration::from_millis(10_500));

    begin_drag_with_landed_preview(&mut harness, first_target);
    extend_drag_forward_to(&mut harness, extended_target);
    release_on_visible_frame(&mut harness);

    assert!(
        harness.session.seek_commit().is_none(),
        "release must land: {:?}",
        harness.session.seek_commit()
    );
    assert_eq!(harness.session.snapshot().current_position, extended_target);
    assert_audio_starts_at_target_without_hole(&log, extended_target);
}

#[test]
fn audio_of_an_uncommitted_drag_waits_compressed_without_blocking_video() {
    let keyframe = Duration::from_millis(7_800);
    let first_target = Duration::from_secs(8);
    let (mut harness, log) = av_scrub_harness(first_target, keyframe, Duration::from_millis(9_000));

    begin_drag_with_landed_preview(&mut harness, first_target);

    // Пока мышь не отпущена, ни одного PCM в output: очищать нечего, терять нечего.
    assert!(audible_segment(&log).is_empty());
    assert!(harness.session.pending_audio_waits_for_live_scrub_commit());
    // Сжатый звук цели лежит в очереди (packets раньше цели отброшены demux-ом).
    assert_eq!(
        harness.session.pipeline.front_pending_audio_packet_start(),
        Some(first_target)
    );
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(first_target)
    );

    release_on_visible_frame(&mut harness);

    assert!(!harness.session.pending_audio_waits_for_live_scrub_commit());
    assert_eq!(harness.seek_requests().len(), 1, "release reused the pass");
    assert_audio_starts_at_target_without_hole(&log, first_target);
}

#[test]
fn slow_fine_drag_keeps_one_decode_pass_with_audio_selected() {
    let keyframe = Duration::from_millis(7_800);
    let first_target = Duration::from_secs(8);
    let (mut harness, log) =
        av_scrub_harness(first_target, keyframe, Duration::from_millis(10_500));

    begin_drag_with_landed_preview(&mut harness, first_target);
    // Медленный точный drag (монтаж): шаги по кадру, каждый меньше декодированного
    // запаса звука. Свойство 12b: проход продолжается без нового seek-а на каждый шаг.
    let mut drag_target = first_target;
    for _ in 0..10 {
        drag_target += VIDEO_FRAME_STEP;
        extend_drag_forward_to(&mut harness, drag_target);
    }
    assert_eq!(
        harness.seek_requests().len(),
        1,
        "every step extended the pass"
    );
    // Сжатый звук до цели drag-а не копится: в очереди он начинается с packet-а цели.
    let queued_audio_start = harness
        .session
        .pipeline
        .front_pending_audio_packet_start()
        .expect("audio of the drag target stays queued");
    assert!(queued_audio_start <= drag_target);
    assert!(drag_target - queued_audio_start < AUDIO_PACKET_DURATION);

    release_on_visible_frame(&mut harness);

    assert_eq!(harness.seek_requests().len(), 1);
    assert_eq!(harness.session.snapshot().current_position, drag_target);
    assert_audio_starts_at_target_without_hole(&log, drag_target);
}

#[test]
fn extension_takes_cold_route_when_written_audio_reaches_past_the_new_target() {
    let keyframe = Duration::from_millis(7_800);
    let first_target = Duration::from_secs(8);
    let extended_target = Duration::from_millis(8_120);
    let (mut harness, log) = av_scrub_harness(first_target, keyframe, Duration::from_millis(9_000));
    begin_drag_with_landed_preview(&mut harness, first_target);
    let generation_before = harness.aligned_seek_commit().generation;
    // Страховка на случай, если PCM прохода всё же записан (обходим удержание и пишем
    // звук 8,0–8,2 с напрямую): extension выбросил бы его безвозвратно.
    for packet_ms in (8_000_u64..8_200).step_by(20) {
        feed_media_time_audio_packet(&mut harness, Duration::from_millis(packet_ms));
    }
    assert!(!audible_segment(&log).is_empty());

    harness
        .session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_duration(extended_target),
        )))
        .unwrap();

    // Cold route: новый demux seek перечитает и звук, и видео новой цели.
    assert_eq!(harness.seek_requests().len(), 2);
    assert!(harness.aligned_seek_commit().generation > generation_before);
    assert_eq!(
        harness.aligned_seek_commit().target_position,
        MediaTime::from_duration(extended_target)
    );
}

#[test]
fn extension_with_written_audio_ending_before_the_new_target_is_kept() {
    let keyframe = Duration::from_millis(7_800);
    let first_target = Duration::from_secs(8);
    let extended_target = Duration::from_millis(8_120);
    let (mut harness, _log) =
        av_scrub_harness(first_target, keyframe, Duration::from_millis(9_000));
    begin_drag_with_landed_preview(&mut harness, first_target);
    // Записанный PCM кончается на 8,1 с (+20 мс шва, меньше допуска) — терять нечего.
    for packet_ms in (8_000_u64..8_100).step_by(20) {
        feed_media_time_audio_packet(&mut harness, Duration::from_millis(packet_ms));
    }

    harness
        .session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_duration(extended_target),
        )))
        .unwrap();

    assert_eq!(harness.seek_requests().len(), 1, "the pass was extended");
}

#[test]
fn release_does_not_reuse_a_pass_whose_audio_starts_after_the_commit_target() {
    let keyframe = Duration::from_millis(7_800);
    let first_target = Duration::from_secs(8);
    let (mut harness, _log) =
        av_scrub_harness(first_target, keyframe, Duration::from_millis(9_000));
    begin_drag_with_landed_preview(&mut harness, first_target);
    // Отрезок звука прохода начат с 8,3 с: звук 8,0–8,3 с уже потерян.
    feed_media_time_audio_packet(&mut harness, Duration::from_millis(8_300));
    assert_eq!(
        harness.session.pipeline.audio_segment_first_sample(),
        Some(Duration::from_millis(8_300))
    );

    harness
        .session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitVisiblePreview,
        ))
        .unwrap();

    // Вместо прохода с дырой — новый seek в ту же цель.
    assert_eq!(harness.seek_requests().len(), 2);
    assert_eq!(
        harness.aligned_seek_commit().target_position,
        MediaTime::from_duration(first_target)
    );
}

/// Пишет audio packet прохода напрямую через decode boundary (в обход удержания).
fn feed_media_time_audio_packet(harness: &mut SeekRegressionHarness, packet_pts: Duration) {
    let generation = harness.session.pipeline.seek_generation();
    let pts_ms = u32::try_from(packet_pts.as_millis()).expect("test pts fits u32");
    harness.session.process_audio_packet(
        AUDIO_TRACK,
        packet_pts,
        None,
        Some(AUDIO_PACKET_DURATION),
        generation,
        &pts_ms.to_le_bytes(),
    );
}

/// Сколько video packets прохода прочитано demux-ом (отправлены decoder-у или ждут).
fn demuxed_video_packets(harness: &SeekRegressionHarness) -> usize {
    harness.sent_packets().len() + harness.session.pipeline.pending_video_packet_len()
}

#[test]
fn held_drag_audio_without_output_does_not_stop_demux_of_the_pass() {
    let keyframe = Duration::from_millis(7_800);
    let first_target = Duration::from_secs(8);
    let (mut harness, log) = av_scrub_harness(first_target, keyframe, Duration::from_millis(9_000));
    harness
        .session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    harness
        .session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_duration(first_target),
        )))
        .unwrap();

    // Кадр цели готов сразу: проход выходит из fast preroll, и дальше обычные tick-и
    // спрашивают demux admission про audio read-ahead. Output-а нет, а pending звук
    // цели ждёт commit: он не должен держать чтение видео.
    harness.push_decoded_frame(first_target, 80, 1);
    for _ in 0..40 {
        let _ = harness.tick_once();
    }

    assert!(harness.session.pending_audio_waits_for_live_scrub_commit());
    assert!(audible_segment(&log).is_empty(), "no PCM before release");
    assert!(harness.session.pipeline.pending_audio_packet_len() > 0);
    // 7,8–8,0 с = 5 video packets до первого звука цели; дальше видео читается.
    assert!(
        demuxed_video_packets(&harness) >= 10,
        "demux stopped at the held audio: {} video packets",
        demuxed_video_packets(&harness)
    );
}

#[test]
fn held_drag_audio_is_bounded_when_video_is_sparse() {
    let keyframe = Duration::from_millis(7_800);
    let first_target = Duration::from_secs(8);
    // Редкое видео (обложка): между кадрами 7,8 и 25 с только звук.
    let mut packets = vec![fake_video_packet_with_keyframe(
        VIDEO_TRACK,
        keyframe,
        PacketKeyframe::Keyframe,
    )];
    let mut audio_pts = keyframe;
    while audio_pts < Duration::from_secs(25) {
        packets.push(media_time_audio_packet(audio_pts));
        audio_pts += AUDIO_PACKET_DURATION;
    }
    packets.push(fake_video_packet_with_keyframe(
        VIDEO_TRACK,
        Duration::from_secs(25),
        PacketKeyframe::Keyframe,
    ));
    let tracks = vec![
        fake_track(VIDEO_TRACK.get(), TrackKind::Video),
        fake_track(AUDIO_TRACK.get(), TrackKind::Audio),
    ];
    let demuxer = scripted_seek_demuxer(tracks.clone(), first_target, keyframe, packets);
    let mut harness = SeekRegressionHarness::new(tracks, demuxer);
    harness
        .session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    harness
        .session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_duration(first_target),
        )))
        .unwrap();
    harness.push_decoded_frame(first_target, 80, 1);

    let tick_config = PlayerTickConfig {
        max_demux_packets_per_tick: 64,
        ..seek_regression_tick_config()
    };
    for _ in 0..40 {
        let _ = harness
            .session
            .tick(PlayerTickContext::with_config(Instant::now(), tick_config));
    }

    // Запас сжатого звука drag-а — тот же предел, что у удержанного звука (10 с).
    let audio_backlog = harness.session.pipeline.pending_audio_backlog();
    assert!(harness.session.pending_audio_waits_for_live_scrub_commit());
    assert!(
        audio_backlog.media_span >= tick_config.startup_video_wait_max_audio_backlog
            && audio_backlog.media_span
                < tick_config.startup_video_wait_max_audio_backlog + AUDIO_PACKET_DURATION,
        "held drag audio must stop demux at the backlog limit: {audio_backlog:?}"
    );
}
