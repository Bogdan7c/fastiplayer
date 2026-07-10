mod support;

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use audio::decoder::{
    AudioDecoderConfig, AudioDecoderHandle, AudioPacketTimeBase, AudioPacketTiming,
    EncodedAudioPacket, create_audio_decoder,
};
use media_core::TimelineNotSeekableReason;
use support::manual_media::{report_selected_media, selected_media_path};
use symphonia_demux::{
    DemuxError, DemuxReadEvent, DemuxSeekRequest, DemuxSeekability, Demuxer, MediaDemuxError,
    Packet, SymphoniaDemuxer, TrackId, TrackInfo, TrackKind,
};

const MAX_EVENTS_BEFORE_AUDIO_PACKET: usize = 256;
const MAX_EMPTY_AUDIO_DECODE_RESULTS: usize = 64;
const MAX_EVENTS_BEFORE_EOF: usize = 16_384;
const MAX_EVENTS_AFTER_REPLAY_SEEK: usize = 256;

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn audio_decode_and_middle_seek_preserve_decodable_pcm() -> Result<()> {
    let path = selected_media_path()?;
    assert_audio_decodes_and_seeks(&path, "audio-decode-seek")
}

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn audio_eof_replay_returns_first_selected_audio_packet() -> Result<()> {
    let path = selected_media_path()?;
    let mut demuxer = open_audio_media(&path, "audio-eof-replay")?;
    let audio_track = first_audio_track(&demuxer)?;
    let tracks_before_eof = demuxer.tracks().to_vec();
    let duration_before_eof = demuxer.duration();
    drain_demuxer_to_real_eof(&mut demuxer, audio_track.id, &path)?;
    let seek_result = demuxer.seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))?;
    ensure!(
        demuxer.tracks() == tracks_before_eof.as_slice(),
        "EOF replay changed the public track layout"
    );
    ensure!(
        demuxer.duration() == duration_before_eof,
        "EOF replay changed the public duration"
    );
    ensure!(
        seek_result.requested_position.as_duration().is_zero(),
        "EOF replay seek did not preserve zero target"
    );
    let replay_packet =
        first_selected_audio_packet_after_replay(&mut demuxer, audio_track.id, &path)?;
    ensure!(
        is_selected_audio_packet(&replay_packet, audio_track.id),
        "EOF replay returned a packet from another track"
    );
    Ok(())
}

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn unseekable_selected_audio_stream_after_eof_stays_unseekable() -> Result<()> {
    // Runner передаёт единственный выбранный пользователем файл без fixed fixture name.
    let path = selected_media_path()?;
    // Extension нужен Symphonia только как probe hint для потокового, принципиально non-seekable source.
    let extension_hint = path
        .extension()
        .and_then(|extension| extension.to_str())
        .context("selected audio stream has no UTF-8 extension hint")?;
    // File передаётся через from_stream, чтобы этот путь не получил capability seekable source.
    let source = File::open(&path)
        .with_context(|| format!("open selected audio stream: {}", path.display()))?;
    // Production streaming boundary должен сохранить typed non-seekable status до и после EOF.
    let mut demuxer = SymphoniaDemuxer::from_stream(source, extension_hint, "manual audio stream")?;
    // Фиксируем публичные metadata до EOF, чтобы failed seek не мог скрытно их изменить.
    let tracks_before_eof = demuxer.tracks().to_vec();
    let duration_before_eof = demuxer.duration();
    // Для проверки именно lifecycle after-EOF дочитываем выбранный audio track до естественного конца.
    let audio_track = first_audio_track(&demuxer)?;
    drain_demuxer_to_real_eof(&mut demuxer, audio_track.id, &path)?;
    // Non-seekable stream после EOF не имеет права внезапно rebuild-нуться как seekable source.
    let seek_error = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
        .expect_err("non-seekable stream must reject seek after EOF");
    // Ошибка остаётся typed demux failure, а не маскируется generic anyhow text или panic.
    ensure!(
        seek_error.downcast_ref::<MediaDemuxError>().is_some()
            || seek_error.downcast_ref::<DemuxError>().is_some(),
        "non-seekable EOF seek returned an untyped error: {seek_error}"
    );
    // Отказ не должен менять lifecycle state, metadata или честную seekability boundary.
    ensure!(
        demuxer.tracks() == tracks_before_eof.as_slice(),
        "failed unseekable EOF seek changed the public track layout"
    );
    ensure!(
        demuxer.duration() == duration_before_eof,
        "failed unseekable EOF seek changed the public duration"
    );
    ensure!(
        matches!(
            demuxer.seekability(),
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable
            }
        ),
        "failed unseekable EOF seek changed source seekability"
    );
    Ok(())
}

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn matroska_opus_end_seek_returns_audio_packet() -> Result<()> {
    let path = selected_media_path()?;
    let mut demuxer = open_audio_media(&path, "audio-matroska-end-seek")?;
    let audio_track = first_audio_track(&demuxer)?;
    ensure!(
        audio_track.codec_id.to_ascii_lowercase().contains("opus"),
        "selected Matroska/WebM audio track is not Opus"
    );
    let seek_target = demuxer
        .duration()
        .or(audio_track.duration)
        .context("selected Matroska/WebM Opus stream has no duration")?;
    ensure!(
        !seek_target.is_zero(),
        "selected Matroska/WebM Opus stream has zero duration"
    );
    let seek_result = demuxer.seek_with_request(DemuxSeekRequest::accurate(seek_target))?;
    ensure!(
        seek_result.requested_position.as_duration() == seek_target,
        "end seek changed the requested public duration"
    );
    let packet = first_selected_audio_packet_after_replay(&mut demuxer, audio_track.id, &path)?;
    ensure!(
        packet.pts <= seek_target,
        "first packet after end seek is after the requested duration"
    );
    Ok(())
}

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn matroska_opus_aggressive_late_seeks_reach_near_target_packets() -> Result<()> {
    let path = selected_media_path()?;
    let mut demuxer = open_audio_media(&path, "audio-matroska-late-seeks")?;
    let audio_track = first_audio_track(&demuxer)?;
    ensure!(
        audio_track.codec_id.to_ascii_lowercase().contains("opus"),
        "selected Matroska/WebM audio track is not Opus"
    );
    for seek_target in [
        Duration::from_secs(6),
        Duration::from_secs(2),
        Duration::from_secs(7),
        Duration::from_secs(1),
        Duration::from_millis(7_900),
        Duration::from_secs(4),
    ] {
        demuxer.seek_with_request(DemuxSeekRequest::accurate(seek_target))?;
        let packet = first_selected_audio_packet_covering_target(
            &mut demuxer,
            audio_track.id,
            seek_target,
            &path,
        )?;
        ensure!(
            packet.pts.saturating_sub(seek_target) <= Duration::from_millis(25),
            "late seek landed too far after target {:?}",
            seek_target
        );
    }
    Ok(())
}

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn wavpack_remains_explicitly_unsupported() -> Result<()> {
    let path = selected_media_path()?;
    report_selected_media("audio-wavpack-unsupported", &path, &[])?;
    match SymphoniaDemuxer::from_file(&path) {
        Ok(_) => bail!(
            "selected WavPack file is now supported; update the explicit audio support decision"
        ),
        Err(DemuxError::UnsupportedFormat(_)) => Ok(()),
        Err(error) => bail!("selected WavPack file returned a non-contract error: {error}"),
    }
}

fn open_audio_media(path: &Path, scenario: &str) -> Result<SymphoniaDemuxer> {
    let demuxer = SymphoniaDemuxer::from_file(path)
        .with_context(|| format!("open selected audio media: {}", path.display()))?;
    report_selected_media(scenario, path, demuxer.tracks())?;
    Ok(demuxer)
}

fn assert_audio_decodes_and_seeks(path: &Path, scenario: &str) -> Result<()> {
    let mut demuxer = open_audio_media(path, scenario)?;
    let audio_track = first_audio_track(&demuxer)?;
    let mut decoder = create_decoder_for_track(&audio_track)?;
    let pre_seek_samples = decode_next_audio_samples(
        &mut demuxer,
        decoder.as_mut(),
        audio_track.id,
        "before seek",
    )?;
    ensure!(
        pre_seek_samples > 0,
        "selected audio media produced no PCM before seek"
    );
    let seek_target = middle_seek_target(&demuxer, &audio_track)?;
    let seek_result = demuxer.seek_with_request(DemuxSeekRequest::accurate(seek_target))?;
    ensure!(
        seek_result.requested_position.as_duration() == seek_target,
        "audio seek changed the requested middle target"
    );
    decoder.reset()?;
    let post_seek_samples =
        decode_next_audio_samples(&mut demuxer, decoder.as_mut(), audio_track.id, "after seek")?;
    ensure!(
        post_seek_samples > 0,
        "selected audio media produced no PCM after seek"
    );
    Ok(())
}

fn drain_demuxer_to_real_eof(
    demuxer: &mut SymphoniaDemuxer,
    selected_audio_track_id: TrackId,
    path: &Path,
) -> Result<()> {
    let mut selected_packets = 0_usize;
    for event_index in 0..MAX_EVENTS_BEFORE_EOF {
        match demuxer
            .next_event()
            .with_context(|| format!("{}: read event #{event_index} before EOF", path.display()))?
        {
            DemuxReadEvent::Packet(packet)
                if is_selected_audio_packet(&packet, selected_audio_track_id) =>
            {
                selected_packets += 1
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::EndOfStream => {
                ensure!(
                    selected_packets > 0,
                    "selected audio track had no packet before EOF"
                );
                return Ok(());
            }
        }
    }
    bail!(
        "{}: demuxer did not reach EOF in {MAX_EVENTS_BEFORE_EOF} events",
        path.display()
    )
}

fn first_selected_audio_packet_after_replay(
    demuxer: &mut SymphoniaDemuxer,
    selected_audio_track_id: TrackId,
    path: &Path,
) -> Result<Packet> {
    for event_index in 0..MAX_EVENTS_AFTER_REPLAY_SEEK {
        match demuxer
            .next_event()
            .with_context(|| format!("{}: read replay event #{event_index}", path.display()))?
        {
            DemuxReadEvent::Packet(packet)
                if is_selected_audio_packet(&packet, selected_audio_track_id) =>
            {
                return Ok(packet);
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::EndOfStream => bail!(
                "{}: EOF arrived before selected audio replay packet",
                path.display()
            ),
        }
    }
    bail!(
        "{}: selected audio replay packet not found in {MAX_EVENTS_AFTER_REPLAY_SEEK} events",
        path.display()
    )
}

fn first_selected_audio_packet_covering_target(
    demuxer: &mut SymphoniaDemuxer,
    selected_audio_track_id: TrackId,
    seek_target: Duration,
    path: &Path,
) -> Result<Packet> {
    for event_index in 0..MAX_EVENTS_AFTER_REPLAY_SEEK {
        match demuxer
            .next_event()
            .with_context(|| format!("{}: read seek event #{event_index}", path.display()))?
        {
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
                bail!("{}: EOF arrived before target audio packet", path.display())
            }
        }
    }
    bail!(
        "{}: target audio packet was not reached in {MAX_EVENTS_AFTER_REPLAY_SEEK} events",
        path.display()
    )
}

fn first_audio_track(demuxer: &SymphoniaDemuxer) -> Result<TrackInfo> {
    demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .cloned()
        .context("selected media has no public audio track")
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
        .context("selected audio media has no duration for middle seek")?;
    ensure!(
        !duration.is_zero(),
        "selected audio media has zero duration"
    );
    Ok(duration / 2)
}

fn decode_next_audio_samples(
    demuxer: &mut SymphoniaDemuxer,
    decoder: &mut dyn audio::decoder::AudioDecoder,
    selected_audio_track_id: TrackId,
    phase: &str,
) -> Result<usize> {
    let mut empty_results = 0_usize;
    for _ in 0..MAX_EVENTS_BEFORE_AUDIO_PACKET {
        match demuxer.next_event()? {
            DemuxReadEvent::Packet(packet)
                if is_selected_audio_packet(&packet, selected_audio_track_id) =>
            {
                let decoded_samples =
                    decoder.decode(&encoded_audio_packet_from_media_packet(&packet))?;
                if !decoded_samples.is_empty() {
                    return Ok(decoded_samples.len());
                }
                empty_results += 1;
                ensure!(
                    empty_results <= MAX_EMPTY_AUDIO_DECODE_RESULTS,
                    "too many empty decode results {phase}"
                );
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::EndOfStream => {
                bail!("EOF arrived before decoded audio samples {phase}")
            }
        }
    }
    bail!("decoded audio samples {phase} were not found in {MAX_EVENTS_BEFORE_AUDIO_PACKET} events")
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
        packet.track_duration.map(|duration| duration.units.get()),
    )
}
