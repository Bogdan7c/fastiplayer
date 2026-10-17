//! Output на время смены формата: новый поток владеет звуком, старый доигрывает хвост.

use super::*;
use crate::pipeline::overlapped_audio_output::OutputWithRetiringTail;
use crate::pipeline::{ReplacementOutputPlayback, SpecChangeClockAnchor, SpecChangeTimeline};
use audio_core::AudioOutputStreamHealth;

/// Наблюдаемое состояние fake output-а после передачи владения pipeline.
#[derive(Default)]
struct RecordedOutputState {
    /// PCM, записанный в этот output.
    written_samples: Vec<f32>,
    /// Сколько раз output запускали.
    play_count: usize,
    /// Сколько раз ставили на паузу.
    pause_count: usize,
    /// Громкости в порядке применения.
    applied_volumes: Vec<f32>,
    /// Generation-ы seek-очисток.
    cleared_generations: Vec<u64>,
    /// Слышимая позиция clock-а.
    audible_position: Duration,
    /// Конец принятого PCM.
    submitted_end_position: Duration,
    /// Backend сообщил о поломке потока.
    stream_failed: bool,
    /// Pause вернёт ошибку.
    pause_fails: bool,
    /// Уровень буфера, который сообщает output.
    buffer_level_ms: f64,
}

/// Общий handle состояния fake output-а и его clock-а.
#[derive(Clone, Default)]
struct RecordedOutputHandle(Arc<Mutex<RecordedOutputState>>);

impl RecordedOutputHandle {
    /// Читает состояние под mutex-ом.
    fn read<T>(&self, read: impl FnOnce(&RecordedOutputState) -> T) -> T {
        read(&self.0.lock().expect("fake output state lock"))
    }

    /// Меняет состояние под mutex-ом.
    fn update(&self, update: impl FnOnce(&mut RecordedOutputState)) {
        update(&mut self.0.lock().expect("fake output state lock"));
    }

    /// Задаёт слышимую позицию и конец принятого PCM.
    fn set_timing(&self, audible_position: Duration, submitted_end_position: Duration) {
        self.update(|state| {
            state.audible_position = audible_position;
            state.submitted_end_position = submitted_end_position;
        });
    }

    /// Создаёт output, который пишет в этот handle.
    fn output(&self) -> Box<dyn PlayerAudioOutput> {
        Box::new(RecordedOutput(self.clone()))
    }
}

/// Fake output, всё видимое состояние которого лежит в [`RecordedOutputHandle`].
struct RecordedOutput(RecordedOutputHandle);

impl PlayerAudioClock for RecordedOutputHandle {
    fn now(&self) -> Duration {
        self.read(|state| state.audible_position)
    }

    fn output_timing(&self) -> AudioOutputClockTiming {
        self.read(|state| {
            AudioOutputClockTiming::new(state.audible_position, state.submitted_end_position)
        })
    }

    fn reset(&self) {
        self.set_timing(Duration::ZERO, Duration::ZERO);
    }

    fn underrun_callbacks(&self) -> u64 {
        0
    }
}

impl PlayerAudioOutput for RecordedOutput {
    fn write_samples(
        &mut self,
        samples: &[f32],
        _intent: AudioOutputWriteIntent,
    ) -> Result<AudioOutputWriteReport, AudioOutputWriteError> {
        self.0
            .update(|state| state.written_samples.extend_from_slice(samples));
        let frames = samples.len() / 2;
        Ok(AudioOutputWriteReport::complete(
            AudioOutputInputFrameCount::new(frames),
            AudioOutputStreamFrameCount::new(frames),
        ))
    }

    fn play(&mut self) -> anyhow::Result<()> {
        self.0.update(|state| state.play_count += 1);
        Ok(())
    }

    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        self.0.update(|state| state.pause_count += 1);
        if self.0.read(|state| state.pause_fails) {
            anyhow::bail!("scripted pause failure");
        }
        Ok(self.0.output_timing())
    }

    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        self.0
            .update(|state| state.cleared_generations.push(generation));
        Ok(generation)
    }

    fn set_volume(&mut self, volume: f32) {
        self.0.update(|state| state.applied_volumes.push(volume));
    }

    fn buffer_level_ms(&self) -> f64 {
        self.0.read(|state| state.buffer_level_ms)
    }

    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        Arc::new(self.0.clone())
    }

    fn stream_health(&self) -> AudioOutputStreamHealth {
        if self.0.read(|state| state.stream_failed) {
            AudioOutputStreamHealth::Failed
        } else {
            AudioOutputStreamHealth::Running
        }
    }
}

/// Хвост старого output-а, который ещё не прозвучал.
const UNPLAYED_TAIL: Duration = Duration::from_millis(200);

/// Новый и старый output с хвостом 200 мс у старого.
fn overlapped_outputs() -> (
    OutputWithRetiringTail,
    RecordedOutputHandle,
    RecordedOutputHandle,
) {
    let active = RecordedOutputHandle::default();
    let retiring = RecordedOutputHandle::default();
    retiring.set_timing(
        Duration::from_secs(1),
        Duration::from_secs(1) + UNPLAYED_TAIL,
    );
    let output = OutputWithRetiringTail::new(active.output(), retiring.output());
    (output, active, retiring)
}

/// Жив ли ещё старый output: громкость доходит только до открытого потока.
fn still_controls(output: &mut OutputWithRetiringTail, retiring: &RecordedOutputHandle) -> bool {
    let probe_volume = 0.123;
    output.set_volume(probe_volume);
    retiring.read(|state| state.applied_volumes.last() == Some(&probe_volume))
}

#[test]
fn new_pcm_reaches_only_active_output_while_previous_keeps_playing_its_tail() {
    let (mut output, active, retiring) = overlapped_outputs();

    let report = output
        .write_samples(&[0.5, -0.5], AudioOutputWriteIntent::DirectDecodedPcm)
        .expect("active fake accepts PCM");

    assert!(report.is_complete());
    assert_eq!(
        active.read(|state| state.written_samples.clone()),
        [0.5, -0.5]
    );
    assert!(retiring.read(|state| state.written_samples.is_empty()));
    assert!(still_controls(&mut output, &retiring));
}

#[test]
fn previous_output_is_released_once_its_tail_has_played() {
    let (mut output, _active, retiring) = overlapped_outputs();
    let tail_end = Duration::from_secs(1) + UNPLAYED_TAIL;

    retiring.set_timing(tail_end, tail_end);
    output
        .write_samples(&[0.0, 0.0], AudioOutputWriteIntent::DirectDecodedPcm)
        .expect("active fake accepts PCM");

    assert!(!still_controls(&mut output, &retiring));
}

#[test]
fn play_and_pause_drive_both_streams_but_timing_belongs_to_active_output() {
    let (mut output, active, retiring) = overlapped_outputs();
    active.set_timing(Duration::from_millis(30), Duration::from_millis(250));

    output.play().expect("both fakes start");
    let paused_timing = output.pause_and_freeze_clock().expect("both fakes pause");

    assert_eq!(
        paused_timing,
        AudioOutputClockTiming::new(Duration::from_millis(30), Duration::from_millis(250))
    );
    assert_eq!(
        active.read(|state| (state.play_count, state.pause_count)),
        (1, 1)
    );
    assert_eq!(
        retiring.read(|state| (state.play_count, state.pause_count)),
        (1, 1)
    );
    assert_eq!(output.clock().now(), Duration::from_millis(30));
}

#[test]
fn seek_clear_releases_previous_output_and_acks_active_generation() {
    let (mut output, active, retiring) = overlapped_outputs();

    let ack = output.clear_buffer_for_seek(7).expect("active fake clears");

    assert_eq!(ack, 7);
    assert_eq!(active.read(|state| state.cleared_generations.clone()), [7]);
    assert!(retiring.read(|state| state.cleared_generations.is_empty()));
    assert!(!still_controls(&mut output, &retiring));
}

#[test]
fn broken_previous_stream_is_released_without_affecting_active_health() {
    let (mut output, active, retiring) = overlapped_outputs();
    active.update(|state| state.buffer_level_ms = 42.0);

    retiring.update(|state| state.stream_failed = true);
    assert!(matches!(
        output.stream_health(),
        AudioOutputStreamHealth::Running
    ));
    output
        .write_samples(&[0.0, 0.0], AudioOutputWriteIntent::DirectDecodedPcm)
        .expect("active fake accepts PCM");

    assert!(!still_controls(&mut output, &retiring));
    assert_eq!(output.buffer_level_ms(), 42.0);
}

#[test]
fn previous_output_control_failure_is_not_a_player_error() {
    let (mut output, active, retiring) = overlapped_outputs();
    retiring.update(|state| state.pause_fails = true);

    let paused = output.pause_and_freeze_clock();

    assert!(paused.is_ok(), "the tail is best effort: {paused:?}");
    assert_eq!(active.read(|state| state.pause_count), 1);
    assert!(!still_controls(&mut output, &retiring));
}

#[test]
fn pipeline_swaps_in_new_format_output_with_explicit_clock_anchor() {
    let mut pipeline = PlaybackPipeline::default();
    let previous = RecordedOutputHandle::default();
    previous.set_timing(
        Duration::from_secs(4),
        Duration::from_secs(4) + UNPLAYED_TAIL,
    );
    let stereo = audio_output_spec_for_tests(48_000, 2);
    let surround = audio_output_spec_for_tests(48_000, 6);
    pipeline.install_audio_output(previous.output(), stereo);
    assert_eq!(
        pipeline.play_audio_output().map(|result| result.is_ok()),
        Some(true)
    );
    let replacement = RecordedOutputHandle::default();
    let playback_rate = PlaybackRate::new(1.5).expect("valid rate");

    pipeline.install_audio_output_after_spec_change(
        replacement.output(),
        surround,
        SpecChangeClockAnchor {
            output_clock_start: Duration::ZERO,
            media_position: Duration::from_secs(10),
            timeline: SpecChangeTimeline::StartAtPlaybackRate(playback_rate),
        },
        ReplacementOutputPlayback::Started,
    );

    assert_eq!(pipeline.audio_output_input_spec(), Some(surround));
    assert!(pipeline.audio_output_is_playing());
    assert!(!pipeline.audio_output_needs_play_request());
    // Тишина длиной хвоста на 1.5x продвигает media на 300 мс от точки стыка.
    replacement.set_timing(UNPLAYED_TAIL, UNPLAYED_TAIL);
    assert_eq!(
        pipeline.media_position_from_audio_clock(),
        Duration::from_millis(10_300)
    );
    // Clock старого output-а больше не влияет на позицию.
    previous.set_timing(Duration::from_secs(9), Duration::from_secs(9));
    assert_eq!(
        pipeline.media_position_from_audio_clock(),
        Duration::from_millis(10_300)
    );
}

#[test]
fn not_started_replacement_waits_for_ordinary_play_request() {
    let mut pipeline = PlaybackPipeline::default();
    let previous = RecordedOutputHandle::default();
    pipeline.install_audio_output(previous.output(), audio_output_spec_for_tests(48_000, 2));
    let replacement = RecordedOutputHandle::default();

    pipeline.install_audio_output_after_spec_change(
        replacement.output(),
        audio_output_spec_for_tests(44_100, 2),
        SpecChangeClockAnchor {
            output_clock_start: Duration::ZERO,
            media_position: Duration::from_secs(3),
            timeline: SpecChangeTimeline::StartAtPlaybackRate(PlaybackRate::NORMAL),
        },
        ReplacementOutputPlayback::NotStarted,
    );

    assert!(!pipeline.audio_output_is_playing());
    assert!(pipeline.audio_output_needs_play_request());
    assert_eq!(
        pipeline.play_audio_output().map(|result| result.is_ok()),
        Some(true)
    );
    assert_eq!(replacement.read(|state| state.play_count), 1);
    assert_eq!(previous.read(|state| state.play_count), 1);
}

/// Перенос mapping-а на новый clock сохраняет rate-сегменты хвоста: 200 мс PCM
/// старого 1x, затем 1.5x — на новой оси сдвинуты ровно на разницу стартов.
#[test]
fn carried_mapping_keeps_tail_rate_segments_on_replacement_clock_axis() {
    use crate::pipeline::audio_clock_mapping::AudioClockMediaMapping;

    let fast = PlaybackRate::new(1.5).expect("valid rate");
    let previous_clock = Duration::from_secs(4);
    let mut mapping =
        AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, PlaybackRate::NORMAL);
    mapping.reanchor_for_rate_change(
        previous_clock,
        previous_clock,
        previous_clock + UNPLAYED_TAIL,
        fast,
    );

    let replacement_start = Duration::from_millis(30);
    mapping.carry_onto_replacement_clock(previous_clock, replacement_start);

    assert_eq!(
        mapping.media_position_at_output_clock(replacement_start),
        previous_clock
    );
    // Хвост проигрывается на 1x...
    assert_eq!(
        mapping.media_position_at_output_clock(replacement_start + UNPLAYED_TAIL),
        previous_clock + UNPLAYED_TAIL
    );
    // ...и только после него действует 1.5x.
    assert_eq!(
        mapping.media_position_at_output_clock(
            replacement_start + UNPLAYED_TAIL + Duration::from_millis(100)
        ),
        previous_clock + UNPLAYED_TAIL + Duration::from_millis(150)
    );
    assert_eq!(mapping.open_playback_rate(), fast);
}
