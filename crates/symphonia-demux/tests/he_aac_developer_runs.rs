// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Прогон разработчика (решения 347–351 плана моста): любой файл HE-AAC с диска
//! через production-цепочку плеера — демуксер → маршрут AAC → декодер — с печатью
//! паспорта, формата каждого буфера и итогов. Для сверки с эталонами (FAAD2,
//! Fraunhofer) PCM можно записать в файл f32le.
//!
//! Файлы сайтов в репозиторий не кладутся, поэтому тест `#[ignore]` и берёт путь
//! из окружения:
//!
//! ```text
//! FASTIPLAYER_HE_AAC_FILE=/путь/к/файлу.mp4 \
//! FASTIPLAYER_HE_AAC_PCM_OUT=/путь/к/pcm.f32 \
//! cargo test -p symphonia-demux --features audio/vendored-ffmpeg \
//!   --test he_aac_developer_runs -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use audio::{AudioDecoderConfig, AudioOutputSpec, EncodedAudioPacket, create_audio_decoder};
use media_core::{DemuxReadEvent, Demuxer, TrackKind};
use symphonia_demux::SymphoniaDemuxer;

/// Путь к файлу HE-AAC.
const INPUT_PATH_VARIABLE: &str = "FASTIPLAYER_HE_AAC_FILE";
/// Необязательный путь, куда записать interleaved PCM f32le.
const PCM_OUTPUT_VARIABLE: &str = "FASTIPLAYER_HE_AAC_PCM_OUT";

#[test]
#[ignore = "developer run: needs FASTIPLAYER_HE_AAC_FILE and FFmpeg (audio/vendored-ffmpeg)"]
fn he_aac_file_from_environment_plays_through_the_production_chain() {
    let input_path =
        PathBuf::from(std::env::var_os(INPUT_PATH_VARIABLE).expect("set FASTIPLAYER_HE_AAC_FILE"));
    let mut demuxer = SymphoniaDemuxer::from_file(&input_path).expect("file opens");
    let track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .expect("audio track")
        .clone();
    let codec_private = track.codec_private.as_ref().map(|bytes| bytes.to_vec());
    println!(
        "track: codec {} container rate {:?} channels {:?} time base {:?} ASC {}",
        track.codec_id,
        track.sample_rate,
        track.channels,
        track.time_base,
        codec_private
            .as_deref()
            .map(|bytes| bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>())
            .unwrap_or_else(|| "none".to_owned())
    );

    let config = AudioDecoderConfig::from_track_metadata(
        track.id.get(),
        track.codec_id.clone(),
        track.sample_rate,
        track.channels,
    )
    .with_codec_private(codec_private);
    let mut decoder = create_audio_decoder(config).expect("decoder for the file");

    let mut pcm = Vec::new();
    let mut output_specs: Vec<AudioOutputSpec> = Vec::new();
    let (mut packets, mut empty_buffers) = (0_usize, 0_usize);
    loop {
        match demuxer.next_event().expect("read event") {
            DemuxReadEvent::Packet(packet) if packet.track_id == track.id => {
                packets += 1;
                let samples = decoder
                    .decode(&EncodedAudioPacket::without_timing(
                        track.id.get(),
                        &packet.data,
                    ))
                    .expect("packet decodes");
                if samples.is_empty() {
                    empty_buffers += 1;
                    continue;
                }
                let spec = decoder.decoded_output_spec().expect("spec after PCM");
                if output_specs.last() != Some(&spec) {
                    println!(
                        "packet {packets}: output {} Hz x{}",
                        spec.sample_rate,
                        spec.channels()
                    );
                    output_specs.push(spec);
                }
                pcm.extend(samples);
            }
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::TemporarilyUnavailable(_) => panic!("local file is always ready"),
            _ => {}
        }
    }

    let spec = *output_specs.first().expect("the file produced PCM");
    let frames = pcm.len() / spec.channels() as usize;
    let peak = pcm
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    println!(
        "packets {packets}, empty buffers {empty_buffers}, PCM {frames} frames = {:.3} s, peak {peak:.3}",
        frames as f64 / f64::from(spec.sample_rate)
    );
    if let Some(output_path) = std::env::var_os(PCM_OUTPUT_VARIABLE) {
        let bytes: Vec<u8> = pcm.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        std::fs::write(&output_path, bytes).expect("PCM written");
        println!(
            "PCM f32le written to {}",
            PathBuf::from(output_path).display()
        );
    }

    assert_eq!(
        output_specs.len(),
        1,
        "one output format from the first buffer"
    );
    assert_eq!(empty_buffers, 0, "no packet was skipped");
    assert!(peak > 0.01, "audio is not silent");
}
