//! UX-11: громкость, восстановленная после перезапуска, доходит до аудиовыхода.
//!
//! App восстанавливает уровень пользователя командами `SetVolume(слышимая)` →
//! `SetVolume(0.0)` ещё до открытия media. Проверяем по факту на fake output-е:
//! output создаётся уже заглушённым, PCM при этом в него идёт, а включение звука
//! возвращает сохранённую громкость, а не fallback из config-а.

use audio_core::AudioPacketTiming;
use bytes::Bytes;
use media_core::PacketPresentationWindow;

use super::audio_packet_window::RecordingPcmDecoder;
use super::test_support::ScriptedAudioOutputFactory;
use super::*;

/// Audio track тестового media.
const AUDIO_TRACK: u32 = 2;

/// PCM, который fake decoder возвращает на packet (stereo, 2 кадра).
const DECODED_PCM: [f32; 4] = [0.25, -0.5, 0.75, -1.0];

/// Громкость пользователя «до mute», сохранённая в config.
const SAVED_AUDIBLE_VOLUME: f32 = 0.3;

/// Fallback mute-toggle (тоже из config), который не должен победить сохранённую громкость.
const UNRELATED_FALLBACK_VOLUME: f32 = 0.8;

#[test]
fn restored_mute_reaches_audio_output_and_unmute_returns_saved_volume() {
    let (factory, factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_output_factory(factory);
    let track_id = TrackId::new(AUDIO_TRACK);
    session.pipeline.select_audio_track(track_id);
    session.snapshot.selected_tracks.audio_track = Some(track_id);
    session
        .pipeline
        .install_audio_decoder(Box::new(RecordingPcmDecoder::new(
            Arc::new(Mutex::new(Vec::new())),
            DECODED_PCM.to_vec(),
            48_000,
            2,
        )));

    // Порядок команд app-слоя при старте с сохранённым mute.
    session
        .dispatch_command(PlayerCommand::SetVolume(SAVED_AUDIBLE_VOLUME))
        .expect("saved volume is valid");
    session
        .dispatch_command(PlayerCommand::SetVolume(0.0))
        .expect("mute volume is valid");
    session.dispatch_command(PlayerCommand::Play).expect("play");
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::with_timing(
            track_id,
            Duration::from_millis(10),
            AudioPacketTiming::unknown(),
            PacketPresentationWindow::Unbounded,
            session.pipeline.seek_generation(),
            Bytes::from_static(b"encoded-audio"),
        ));
    session.process_pending_audio_packets_with_buffer_limit(200.0);

    let output = factory_handle
        .last_output_handle()
        .expect("первый packet создаёт audio output");
    assert_eq!(output.written_samples(), DECODED_PCM.to_vec());
    assert_eq!(
        output.last_applied_volume(),
        Some(0.0),
        "output создан заглушённым"
    );
    assert!(session.snapshot().muted);

    session
        .dispatch_command(PlayerCommand::ToggleMute {
            fallback_volume: UNRELATED_FALLBACK_VOLUME,
        })
        .expect("unmute");

    assert_eq!(output.last_applied_volume(), Some(SAVED_AUDIBLE_VOLUME));
    assert!(!session.snapshot().muted);
    assert_eq!(session.snapshot().volume, SAVED_AUDIBLE_VOLUME);
}
