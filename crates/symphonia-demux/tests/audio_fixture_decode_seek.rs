use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use audio::decoder::{
    AudioDecoderConfig, AudioDecoderHandle, AudioPacketTimeBase, AudioPacketTiming,
    EncodedAudioPacket, create_audio_decoder,
};
use symphonia_demux::{
    DemuxError, DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, SymphoniaDemuxer, TrackId,
    TrackInfo, TrackKind,
};

/// Верхняя граница защищает тест от бесконечного чтения при regressions в demux loop.
const MAX_EVENTS_BEFORE_AUDIO_PACKET: usize = 256;

/// Несколько пустых decode результатов допустимы, но долгий поток пустоты означает regression.
const MAX_EMPTY_AUDIO_DECODE_RESULTS: usize = 64;

/// Один fixture из headless audio acceptance matrix.
#[derive(Debug, Clone, Copy)]
struct AudioFixtureCase {
    /// Человеческая метка формата, которую показываем в diagnostics теста.
    label: &'static str,

    /// Реальный файл в `test-assets/audio`; ALAC живёт в `.m4a` container-е.
    file_name: &'static str,

    /// Ожидаемый contract для текущего Symphonia/open path.
    expectation: AudioFixtureExpectation,
}

/// Явно разделяет working formats и формат, решение по которому ещё не принято.
#[derive(Debug, Clone, Copy)]
enum AudioFixtureExpectation {
    /// Fixture должен открыться, декодироваться, выполнить accurate seek и декодироваться снова.
    DecodeSeek,

    /// Standalone `.wv` сейчас не открывается текущим Symphonia probe path.
    UnsupportedInCurrentSymphoniaPath,
}

/// Матрица фиксирует именно runtime поведение fixtures, а не только список UI extensions.
const AUDIO_FIXTURE_CASES: &[AudioFixtureCase] = &[
    AudioFixtureCase::decode_seek("wav", "music_sample.wav"),
    AudioFixtureCase::decode_seek("aiff", "music_sample.aiff"),
    AudioFixtureCase::decode_seek("aif", "music_sample.aif"),
    AudioFixtureCase::decode_seek("caf", "music_sample.caf"),
    AudioFixtureCase::decode_seek("flac", "music_sample.flac"),
    AudioFixtureCase::decode_seek("mp1", "music_sample.mp1"),
    AudioFixtureCase::decode_seek("mp2", "music_sample.mp2"),
    AudioFixtureCase::decode_seek("mp3", "music_sample.mp3"),
    AudioFixtureCase::decode_seek("m4a", "music_sample.m4a"),
    AudioFixtureCase::decode_seek("mp4", "music_sample.mp4"),
    AudioFixtureCase::decode_seek("alac", "music_sample_alac.m4a"),
    AudioFixtureCase::decode_seek("ogg", "music_sample.ogg"),
    AudioFixtureCase::decode_seek("oga", "music_sample.oga"),
    AudioFixtureCase::decode_seek("opus", "music_sample.opus"),
    AudioFixtureCase::decode_seek("webm", "music_sample.webm"),
    AudioFixtureCase::decode_seek("mkv", "music_sample.mkv"),
    AudioFixtureCase::unsupported("wv", "music_sample.wv"),
];

/// Верхняя граница для полного чтения коротких audio fixtures до реального EOF.
const MAX_EVENTS_BEFORE_EOF: usize = 16_384;

/// Верхняя граница для поиска первого выбранного audio packet после replay seek.
const MAX_EVENTS_AFTER_REPLAY_SEEK: usize = 256;

/// Один fixture для focused EOF/replay regression matrix.
#[derive(Debug, Clone, Copy)]
struct AudioEofReplayFixtureCase {
    /// Человеческая метка формата, которую показываем в diagnostics теста.
    label: &'static str,

    /// Реальный файл в `test-assets/audio`.
    file_name: &'static str,
}

impl AudioEofReplayFixtureCase {
    /// Создаёт fixture case для сценария `EOF -> seek(0) -> first audio packet`.
    const fn new(label: &'static str, file_name: &'static str) -> Self {
        Self { label, file_name }
    }
}

/// Матрица целенаправленно покрывает known-bad containers и positive controls.
const EOF_REPLAY_FIXTURE_CASES: &[AudioEofReplayFixtureCase] = &[
    AudioEofReplayFixtureCase::new("alac m4a", "music_sample_alac.m4a"),
    AudioEofReplayFixtureCase::new("m4a", "music_sample.m4a"),
    AudioEofReplayFixtureCase::new("mp4", "music_sample.mp4"),
    AudioEofReplayFixtureCase::new("mkv", "music_sample.mkv"),
    AudioEofReplayFixtureCase::new("webm", "music_sample.webm"),
    AudioEofReplayFixtureCase::new("wav", "music_sample.wav"),
    AudioEofReplayFixtureCase::new("flac", "music_sample.flac"),
    AudioEofReplayFixtureCase::new("mp3", "music_sample.mp3"),
    AudioEofReplayFixtureCase::new("ogg", "music_sample.ogg"),
    AudioEofReplayFixtureCase::new("opus", "music_sample.opus"),
    AudioEofReplayFixtureCase::new("maple leaf rag ogg", "source_maple_leaf_rag.ogg"),
];

/// Matroska/WebM Opus fixtures покрывают поздние seek-и после последней рабочей cue-точки.
const MATROSKA_OPUS_END_SEEK_FIXTURE_CASES: &[AudioEofReplayFixtureCase] = &[
    AudioEofReplayFixtureCase::new("mkv", "music_sample.mkv"),
    AudioEofReplayFixtureCase::new("webm", "music_sample.webm"),
];

impl AudioFixtureCase {
    const fn decode_seek(label: &'static str, file_name: &'static str) -> Self {
        Self {
            label,
            file_name,
            expectation: AudioFixtureExpectation::DecodeSeek,
        }
    }

    const fn unsupported(label: &'static str, file_name: &'static str) -> Self {
        Self {
            label,
            file_name,
            expectation: AudioFixtureExpectation::UnsupportedInCurrentSymphoniaPath,
        }
    }
}

#[test]
fn audio_fixtures_decode_before_and_after_accurate_middle_seek() -> Result<()> {
    if skip_when_audio_fixture_root_is_absent("decode/seek matrix") {
        return Ok(());
    }

    for fixture_case in AUDIO_FIXTURE_CASES {
        run_audio_fixture_case(*fixture_case)
            .with_context(|| format!("audio fixture '{}' failed", fixture_case.label))?;
    }

    Ok(())
}

#[test]
fn audio_fixtures_replay_after_real_eof_returns_first_audio_packet() -> Result<()> {
    if skip_when_audio_fixture_root_is_absent("EOF/replay matrix") {
        return Ok(());
    }

    let mut fixture_failures = Vec::new();

    for fixture_case in EOF_REPLAY_FIXTURE_CASES {
        let fixture_result = assert_fixture_replays_after_eof(*fixture_case).with_context(|| {
            format!(
                "audio EOF/replay fixture '{}' ({}) failed",
                fixture_case.label, fixture_case.file_name
            )
        });

        if let Err(error) = fixture_result {
            fixture_failures.push(format!("{error:#}"));
        }
    }

    ensure!(
        fixture_failures.is_empty(),
        "EOF/replay regression failures:\n{}",
        fixture_failures.join("\n\n")
    );

    Ok(())
}

#[test]
fn matroska_opus_fixtures_seek_to_public_duration_returns_audio_packet() -> Result<()> {
    if skip_when_audio_fixture_root_is_absent("Matroska Opus end-seek matrix") {
        return Ok(());
    }

    let mut fixture_failures = Vec::new();

    for fixture_case in MATROSKA_OPUS_END_SEEK_FIXTURE_CASES {
        let fixture_result =
            assert_fixture_seeks_to_public_duration(*fixture_case).with_context(|| {
                format!(
                    "Matroska Opus end-seek fixture '{}' ({}) failed",
                    fixture_case.label, fixture_case.file_name
                )
            });

        if let Err(error) = fixture_result {
            fixture_failures.push(format!("{error:?}"));
        }
    }

    ensure!(
        fixture_failures.is_empty(),
        "Matroska Opus end-seek regressions:\n{}",
        fixture_failures.join("\n\n")
    );

    Ok(())
}

#[test]
fn matroska_opus_fixtures_aggressive_late_seeks_reach_target_audio_packets() -> Result<()> {
    if skip_when_audio_fixture_root_is_absent("Matroska Opus aggressive seek matrix") {
        return Ok(());
    }

    let mut fixture_failures = Vec::new();

    for fixture_case in MATROSKA_OPUS_END_SEEK_FIXTURE_CASES {
        let fixture_result = assert_fixture_handles_aggressive_late_seeks(*fixture_case)
            .with_context(|| {
                format!(
                    "Matroska Opus aggressive seek fixture '{}' ({}) failed",
                    fixture_case.label, fixture_case.file_name
                )
            });

        if let Err(error) = fixture_result {
            fixture_failures.push(format!("{error:?}"));
        }
    }

    ensure!(
        fixture_failures.is_empty(),
        "Matroska Opus aggressive seek regressions:\n{}",
        fixture_failures.join("\n\n")
    );

    Ok(())
}

fn run_audio_fixture_case(fixture_case: AudioFixtureCase) -> Result<()> {
    let fixture_path = audio_fixture_path(fixture_case.file_name);
    ensure!(
        fixture_path.exists(),
        "fixture '{}' должен существовать: {}",
        fixture_case.label,
        fixture_path.display()
    );

    match fixture_case.expectation {
        AudioFixtureExpectation::DecodeSeek => assert_fixture_decodes_and_seeks(&fixture_path),
        AudioFixtureExpectation::UnsupportedInCurrentSymphoniaPath => {
            assert_fixture_stays_marked_unsupported(fixture_case, &fixture_path)
        }
    }
}

fn assert_fixture_replays_after_eof(fixture_case: AudioEofReplayFixtureCase) -> Result<()> {
    let fixture_path = audio_fixture_path(fixture_case.file_name);
    ensure!(
        fixture_path.exists(),
        "{}: before EOF: fixture должен существовать: {}",
        fixture_case.file_name,
        fixture_path.display()
    );

    let mut demuxer = SymphoniaDemuxer::from_file(&fixture_path).with_context(|| {
        format!(
            "{}: before EOF: open through SymphoniaDemuxer::from_file",
            fixture_case.file_name
        )
    })?;
    let audio_track = first_audio_track(&demuxer).with_context(|| {
        format!(
            "{}: before EOF: find selected audio track",
            fixture_case.file_name
        )
    })?;

    drain_demuxer_to_real_eof(&mut demuxer, audio_track.id, fixture_case.file_name)?;

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
        .with_context(|| {
            format!(
                "{}: seek after EOF: accurate seek to zero",
                fixture_case.file_name
            )
        })?;
    ensure!(
        seek_result.requested_position.as_duration() == Duration::ZERO,
        "{}: seek after EOF: requested_position должен остаться zero",
        fixture_case.file_name
    );

    let replay_packet = first_selected_audio_packet_after_replay(
        &mut demuxer,
        audio_track.id,
        fixture_case.file_name,
    )?;
    ensure!(
        is_selected_audio_packet(&replay_packet, audio_track.id),
        "{}: first packet after replay: packet должен принадлежать выбранному audio track",
        fixture_case.file_name
    );

    Ok(())
}

fn assert_fixture_seeks_to_public_duration(fixture_case: AudioEofReplayFixtureCase) -> Result<()> {
    let fixture_path = audio_fixture_path(fixture_case.file_name);
    let mut demuxer = SymphoniaDemuxer::from_file(&fixture_path).with_context(|| {
        format!(
            "{}: open through SymphoniaDemuxer::from_file",
            fixture_case.file_name
        )
    })?;
    let audio_track = first_audio_track(&demuxer)
        .with_context(|| format!("{}: find selected audio track", fixture_case.file_name))?;
    let seek_target = demuxer
        .duration()
        .or(audio_track.duration)
        .with_context(|| format!("{}: public duration is required", fixture_case.file_name))?;

    ensure!(
        !seek_target.is_zero(),
        "{}: public duration must be non-zero",
        fixture_case.file_name
    );

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(seek_target))
        .with_context(|| {
            format!(
                "{}: accurate seek to public duration {:?}",
                fixture_case.file_name, seek_target
            )
        })?;

    ensure!(
        seek_result.requested_position.as_duration() == seek_target,
        "{}: demuxer must preserve requested public duration",
        fixture_case.file_name
    );

    let packet = first_selected_audio_packet_after_replay(
        &mut demuxer,
        audio_track.id,
        fixture_case.file_name,
    )?;

    ensure!(
        packet.pts <= seek_target,
        "{}: first packet after end seek must not be after requested duration",
        fixture_case.file_name
    );

    Ok(())
}

fn assert_fixture_handles_aggressive_late_seeks(
    fixture_case: AudioEofReplayFixtureCase,
) -> Result<()> {
    let fixture_path = audio_fixture_path(fixture_case.file_name);
    let mut demuxer = SymphoniaDemuxer::from_file(&fixture_path).with_context(|| {
        format!(
            "{}: open through SymphoniaDemuxer::from_file",
            fixture_case.file_name
        )
    })?;
    let audio_track = first_audio_track(&demuxer)
        .with_context(|| format!("{}: find selected audio track", fixture_case.file_name))?;

    for seek_target in [
        Duration::from_secs(6),
        Duration::from_secs(2),
        Duration::from_secs(7),
        Duration::from_secs(1),
        Duration::from_millis(7_900),
        Duration::from_secs(4),
    ] {
        demuxer
            .seek_with_request(DemuxSeekRequest::accurate(seek_target))
            .with_context(|| {
                format!(
                    "{}: accurate seek to {:?}",
                    fixture_case.file_name, seek_target
                )
            })?;

        let packet = first_selected_audio_packet_covering_target(
            &mut demuxer,
            audio_track.id,
            seek_target,
            fixture_case.file_name,
        )?;
        let packet_late_by = packet.pts.saturating_sub(seek_target);

        ensure!(
            packet_late_by <= Duration::from_millis(25),
            "{}: first packet after seek is too far after target ({:?})",
            fixture_case.file_name,
            packet_late_by
        );
    }

    Ok(())
}

fn first_selected_audio_packet_covering_target(
    demuxer: &mut SymphoniaDemuxer,
    selected_audio_track_id: TrackId,
    seek_target: Duration,
    file_name: &str,
) -> Result<Packet> {
    for event_index in 0..MAX_EVENTS_AFTER_REPLAY_SEEK {
        match demuxer.next_event().with_context(|| {
            format!("{file_name}: target packet after seek: read demux event #{event_index}")
        })? {
            DemuxReadEvent::Packet(packet)
                if is_selected_audio_packet(&packet, selected_audio_track_id) =>
            {
                let packet_end = packet
                    .duration
                    .map(|duration| packet.pts + duration)
                    .unwrap_or(packet.pts);

                if packet_end >= seek_target {
                    return Ok(packet);
                }
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::EndOfStream => {
                bail!("{file_name}: target packet after seek: EOF before target audio packet");
            }
        }
    }

    bail!(
        "{file_name}: target packet after seek: target {:?} not reached in {} events",
        seek_target,
        MAX_EVENTS_AFTER_REPLAY_SEEK
    )
}

fn assert_fixture_decodes_and_seeks(fixture_path: &Path) -> Result<()> {
    let mut demuxer = SymphoniaDemuxer::from_file(fixture_path).with_context(|| {
        format!(
            "open through SymphoniaDemuxer::from_file: {}",
            fixture_path.display()
        )
    })?;
    let audio_track = first_audio_track(&demuxer)
        .with_context(|| format!("find audio track in {}", fixture_path.display()))?;
    let mut decoder = create_decoder_for_track(&audio_track)
        .with_context(|| format!("create audio decoder for {}", fixture_path.display()))?;

    let pre_seek_sample_count = decode_next_audio_samples(
        &mut demuxer,
        decoder.as_mut(),
        audio_track.id,
        "before seek",
    )
    .with_context(|| format!("decode first audio samples in {}", fixture_path.display()))?;
    ensure!(
        pre_seek_sample_count > 0,
        "decode до seek должен вернуть хотя бы один PCM sample"
    );

    let seek_target = middle_seek_target(&demuxer, &audio_track)
        .with_context(|| format!("choose middle seek target for {}", fixture_path.display()))?;
    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(seek_target))
        .with_context(|| {
            format!(
                "accurate seek to {:?} in {}",
                seek_target,
                fixture_path.display()
            )
        })?;
    ensure!(
        seek_result.requested_position.as_duration() == seek_target,
        "demuxer должен сохранить requested_position для accurate seek"
    );

    decoder.reset().with_context(|| {
        format!(
            "reset audio decoder after seek in {}",
            fixture_path.display()
        )
    })?;
    let post_seek_sample_count =
        decode_next_audio_samples(&mut demuxer, decoder.as_mut(), audio_track.id, "after seek")
            .with_context(|| {
                format!(
                    "decode audio samples after seek in {}",
                    fixture_path.display()
                )
            })?;
    ensure!(
        post_seek_sample_count > 0,
        "decode после seek должен вернуть хотя бы один PCM sample"
    );

    Ok(())
}

fn drain_demuxer_to_real_eof(
    demuxer: &mut SymphoniaDemuxer,
    selected_audio_track_id: TrackId,
    file_name: &str,
) -> Result<()> {
    let mut selected_audio_packets_before_eof = 0usize;

    for event_index in 0..MAX_EVENTS_BEFORE_EOF {
        match demuxer
            .next_event()
            .with_context(|| format!("{file_name}: before EOF: read demux event #{event_index}"))?
        {
            DemuxReadEvent::Packet(packet)
                if is_selected_audio_packet(&packet, selected_audio_track_id) =>
            {
                selected_audio_packets_before_eof += 1;
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::EndOfStream => {
                ensure!(
                    selected_audio_packets_before_eof > 0,
                    "{file_name}: before EOF: EOF пришёл до первого packet выбранного audio track"
                );
                return Ok(());
            }
        }
    }

    bail!(
        "{file_name}: before EOF: demuxer не дошёл до EOF за {} events",
        MAX_EVENTS_BEFORE_EOF
    )
}

fn first_selected_audio_packet_after_replay(
    demuxer: &mut SymphoniaDemuxer,
    selected_audio_track_id: TrackId,
    file_name: &str,
) -> Result<Packet> {
    for event_index in 0..MAX_EVENTS_AFTER_REPLAY_SEEK {
        match demuxer.next_event().with_context(|| {
            format!("{file_name}: first packet after replay: read demux event #{event_index}")
        })? {
            DemuxReadEvent::Packet(packet)
                if is_selected_audio_packet(&packet, selected_audio_track_id) =>
            {
                return Ok(packet);
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::EndOfStream => {
                bail!(
                    "{file_name}: first packet after replay: EOF пришёл до packet выбранного audio track"
                );
            }
        }
    }

    bail!(
        "{file_name}: first packet after replay: не найден packet выбранного audio track за {} events",
        MAX_EVENTS_AFTER_REPLAY_SEEK
    )
}

fn assert_fixture_stays_marked_unsupported(
    fixture_case: AudioFixtureCase,
    fixture_path: &Path,
) -> Result<()> {
    match SymphoniaDemuxer::from_file(fixture_path) {
        Ok(_) => bail!(
            "fixture '{}' ({}) теперь открывается; нужно явно решить, добавляем ли .wv в supported matrix или меняем contract",
            fixture_case.label,
            fixture_path.display()
        ),
        Err(DemuxError::UnsupportedFormat(_)) => Ok(()),
        Err(error) => bail!(
            "fixture '{}' ({}) должен быть явно unsupported, но вернул другую ошибку: {error}",
            fixture_case.label,
            fixture_path.display()
        ),
    }
}

fn first_audio_track(demuxer: &SymphoniaDemuxer) -> Result<TrackInfo> {
    demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .cloned()
        .context("fixture должен иметь хотя бы один public audio track")
}

fn create_decoder_for_track(audio_track: &TrackInfo) -> Result<AudioDecoderHandle> {
    let decoder_config = AudioDecoderConfig::from_track_metadata(
        audio_track.id.get(),
        audio_track.codec_id.clone(),
        audio_track.sample_rate,
        audio_track.channels,
    )
    .with_codec_private(
        audio_track
            .codec_private
            .as_ref()
            .map(|bytes| bytes.to_vec()),
    );

    create_audio_decoder(decoder_config)
}

fn middle_seek_target(demuxer: &SymphoniaDemuxer, audio_track: &TrackInfo) -> Result<Duration> {
    let duration = demuxer
        .duration()
        .or(audio_track.duration)
        .context("fixture должен сообщать duration для middle seek")?;
    ensure!(
        !duration.is_zero(),
        "fixture duration должен быть ненулевым для middle seek"
    );

    Ok(duration / 2)
}

fn decode_next_audio_samples(
    demuxer: &mut SymphoniaDemuxer,
    decoder: &mut dyn audio::decoder::AudioDecoder,
    selected_audio_track_id: TrackId,
    phase: &'static str,
) -> Result<usize> {
    let mut empty_decode_results = 0;

    for _ in 0..MAX_EVENTS_BEFORE_AUDIO_PACKET {
        match demuxer.next_event()? {
            DemuxReadEvent::Packet(packet)
                if is_selected_audio_packet(&packet, selected_audio_track_id) =>
            {
                let encoded_packet = encoded_audio_packet_from_media_packet(&packet);
                let decoded_samples = decoder.decode(&encoded_packet)?;

                if !decoded_samples.is_empty() {
                    return Ok(decoded_samples.len());
                }

                empty_decode_results += 1;
                ensure!(
                    empty_decode_results <= MAX_EMPTY_AUDIO_DECODE_RESULTS,
                    "слишком много пустых decode результатов {phase}"
                );
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::EndOfStream => {
                bail!("demuxer дошёл до EOF до decoded audio samples {phase}");
            }
        }
    }

    bail!(
        "не удалось получить decoded audio samples {phase} за {} demux events",
        MAX_EVENTS_BEFORE_AUDIO_PACKET
    )
}

fn is_selected_audio_packet(packet: &Packet, selected_audio_track_id: TrackId) -> bool {
    packet.kind == TrackKind::Audio && packet.track_id == selected_audio_track_id
}

fn encoded_audio_packet_from_media_packet(packet: &Packet) -> EncodedAudioPacket<'_> {
    EncodedAudioPacket::new(
        packet.track_id.get(),
        audio_packet_timing_from_media_packet(packet),
        &packet.data,
    )
}

fn audio_packet_timing_from_media_packet(packet: &Packet) -> AudioPacketTiming {
    let Some(track_pts) = packet.track_pts else {
        return AudioPacketTiming::unknown();
    };
    let Some(time_base) =
        AudioPacketTimeBase::new(track_pts.time_base.numer, track_pts.time_base.denom)
    else {
        return AudioPacketTiming::unknown();
    };

    AudioPacketTiming::from_track_units(
        time_base,
        track_pts.units.get(),
        packet.track_dts.map(|track_dts| track_dts.units.get()),
        packet
            .track_duration
            .map(|track_duration| track_duration.units.get()),
    )
}

fn audio_fixture_path(file_name: &str) -> PathBuf {
    audio_fixture_root().join(file_name)
}

fn audio_fixture_root() -> PathBuf {
    workspace_root().join("test-assets/audio")
}

fn skip_when_audio_fixture_root_is_absent(test_label: &str) -> bool {
    let fixture_root = audio_fixture_root();
    if fixture_root.is_dir() {
        return false;
    }

    eprintln!(
        "skipping {test_label}: audio fixture directory is absent: {}",
        fixture_root.display()
    );
    true
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("symphonia-demux crate должен находиться в workspace/crates")
        .to_path_buf()
}
