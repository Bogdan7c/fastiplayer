//! Отдельная real-runtime проверка AUD-005: потеря packet completion ACK.

#![cfg(feature = "ffmpeg")]

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use codec_core::{H264Packetization, VideoCodec, VideoDecodeRequirement};
use demux_api::DemuxInput;
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, TrackId, TrackKind};
use mpeg_ts_demux::{MpegTsDemuxOptions, MpegTsDemuxer};
use source_core::{CancellationToken, LocalFileSource};
use video_core::{
    DecodePacket, DecodeSendError, VideoDecoderActivityWaitOutcome,
    VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState,
    VideoDecoderThreadConfig, VideoStreamConfigResult, VideoStreamDecodeConfig,
    VideoStreamPacketization,
};
use video_ffmpeg::FfmpegSoftwareVideoBackendFactory;
use video_frame_contract::VideoFrameContract;

/// Минимальная packet capacity, которую до исправления наследовал ACK channel.
const MIN_PACKET_CHANNEL_CAPACITY: usize = 1;

/// Burst больше ACK capacity и достаточно длинный для frame-threading delay.
const POST_SEEK_PACKET_BURST: usize = 16;

/// Bounded deadline отделяет доказанную потерю ACK от зависшего decoder worker-а.
const DECODER_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Accurate-seek target оставляет в четырёхсекундном fixture достаточно packets до EOF.
const SEEK_TARGET: Duration = Duration::from_secs(2);

/// Открывает explicit MPEG-TS fixture через production demux boundary.
fn open_demuxer(asset_path: &Path) -> MpegTsDemuxer {
    // LocalFileSource сохраняет тот же seekable byte-source contract, что и player.
    let local_source = LocalFileSource::open(asset_path).expect("открыть AUD-005 MPEG-TS fixture");

    // Production MPEG-TS parser формирует реальные H.264 Annex-B access units.
    MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(local_source)),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("открыть production MPEG-TS demuxer для AUD-005")
}

/// Выполняет decode-safe seek и возвращает ограниченный post-seek packet burst.
fn collect_post_seek_video_packets(asset_path: &Path) -> (TrackId, Duration, Vec<Packet>) {
    // Новый demux owner начинает отдельную accurate-seek сессию.
    let mut demuxer = open_demuxer(asset_path);

    // Stream configuration использует фактический video track выбранного fixture-а.
    let video_track_id = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .map(|track| track.id)
        .expect("AUD-005 fixture должен содержать video track");

    // Decode-point-before соответствует player preroll: декодирование стартует с keyframe.
    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(SEEK_TARGET))
        .expect("выполнить AUD-005 decode-safe seek");

    // Decode point обязан находиться не позже accurate target, чтобы preroll был осмысленным.
    assert!(
        seek_result.actual_position.as_duration() <= SEEK_TARGET,
        "decode-safe seek не должен перескочить target: actual={:?}, target={SEEK_TARGET:?}",
        seek_result.actual_position.as_duration(),
    );

    // Собираем только ограниченный burst, не подменяя production packet parsing.
    let mut post_seek_packets = Vec::with_capacity(POST_SEEK_PACKET_BURST);

    // Читаем до нужного числа video packets либо реального container EOF.
    while post_seek_packets.len() < POST_SEEK_PACKET_BURST {
        // Каждый event приходит из production demux state machine.
        match demuxer
            .next_event()
            .expect("прочитать post-seek MPEG-TS event")
        {
            // Audio/metadata события не относятся к video completion accounting.
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                post_seek_packets.push(packet);
            }
            // Короткий fixture допустим, но обязан дать больше ACK capacity packets.
            DemuxReadEvent::EndOfStream => break,
            // Остальные события не меняют выбранный video packet burst.
            _ => {}
        }
    }

    // Тест обязан действительно создать переполнение ACK channel, а не проверить capacity=1 одним packet-ом.
    assert!(
        post_seek_packets.len() > MIN_PACKET_CHANNEL_CAPACITY,
        "post-seek burst должен быть больше прежней ACK capacity: packets={}, capacity={MIN_PACKET_CHANNEL_CAPACITY}",
        post_seek_packets.len(),
    );

    // Возвращаем фактическую decode point позицию для evidence log-а.
    (
        video_track_id,
        seek_result.actual_position.as_duration(),
        post_seek_packets,
    )
}

/// Запускает production FFmpeg worker с минимальной packet/ACK capacity.
fn open_decoder(
    video_track_id: TrackId,
) -> Box<video_backend_api::VideoBackendDecoderThreadHandle> {
    // Production spawn создаёт packet и ACK channels одинаковой capacity.
    let decoder_thread_config = VideoDecoderThreadConfig {
        // Только исследуемый queue limit отличается от neutral defaults.
        packet_channel_frames: MIN_PACKET_CHANNEL_CAPACITY,
        // Остальные runtime limits сохраняют production default semantics.
        ..VideoDecoderThreadConfig::default()
    };

    // Factory остаётся обычным composition boundary, без test double worker-а.
    let started_backend =
        FfmpegSoftwareVideoBackendFactory::new_with_decoder_config(decoder_thread_config)
            .start_for_composition()
            .expect("запустить production software FFmpeg backend для AUD-005");

    // Проверяем, что тест не был незаметно перенаправлен в другой backend.
    assert_eq!(started_backend.backend_id(), "ffmpeg-sw");

    // Playback-facing handle читает тот же ACK receiver, что и player-core.
    let decoder = started_backend.into_decoder_thread();

    // Fixture использует H.264 Annex-B из production MPEG-TS demuxer-а.
    let decode_requirement = VideoDecodeRequirement::new(VideoCodec::H264);

    // Нейтральный stream contract не раскрывает raw FFmpeg типы наружу.
    let stream_config = VideoStreamDecodeConfig::from_requirement(
        video_track_id,
        &decode_requirement,
        VideoFrameContract::host_yuv420_planar8(),
    )
    .with_packetization(Some(VideoStreamPacketization::H264(
        H264Packetization::AnnexB,
    )));

    // Реальный codec context должен открыться до измерения packet accounting.
    assert_eq!(
        decoder.configure_stream(stream_config),
        VideoStreamConfigResult::Configured,
    );

    // Возвращаем только public decoder boundary.
    decoder
}

/// Переводит production demux packet в нейтральный decoder packet без потери timing metadata.
fn decode_packet_from_demux(packet: Packet, generation: u64) -> DecodePacket {
    // Поля повторяют player-core boundary и не обходят FFmpeg worker queue.
    DecodePacket {
        track_id: packet.track_id,
        pts: packet.pts,
        dts: packet.dts,
        track_pts: packet.track_pts,
        track_dts: packet.track_dts,
        generation,
        encoded_bytes: packet.data,
        keyframe: packet.keyframe.is_known_keyframe(),
        resolved_color: None,
    }
}

/// Освобождает все materialized frames обычным renderer-facing release path-ом.
fn drain_and_release_frames(decoder: &video_backend_api::VideoBackendDecoderThreadHandle) -> usize {
    // Счётчик позволяет выделить packets, завершившиеся без output frame-а.
    let mut released_frames = 0usize;

    // Pool backpressure не должен маскировать исследуемый ACK backpressure.
    while let Some(frame) = decoder.try_recv_frame() {
        // Release сохраняет production ownership/lifecycle AVFrame resource-а.
        decoder.release_frame(frame.resource_handle);

        // Saturating arithmetic исключает нерелевантный overflow в stress evidence.
        released_frames = released_frames.saturating_add(1);
    }

    // Возвращаем число frame outputs именно за текущий drain interval.
    released_frames
}

/// Принимает packet и ждёт его completion, намеренно не читая accumulator.
fn send_and_wait_without_draining_ack(
    decoder: &video_backend_api::VideoBackendDecoderThreadHandle,
    packet: DecodePacket,
) -> usize {
    // Snapshot перед send отделяет activity этого packet-а от configure_stream.
    let activity_snapshot = decoder.decoder_activity_snapshot();

    // Доступный FFmpeg notifier обязан публиковать монотонный epoch.
    let observed_epoch = activity_snapshot
        .captured_epoch()
        .expect("production FFmpeg decoder должен иметь activity notifier");

    // Реальный completion даёт один pulse внутри send/receive loop и один после
    // worker publication; Deferred branch до retry даёт только внешний pulse.
    let completion_epoch_value = observed_epoch.get().saturating_add(2);

    // Успешный send — ровно тот момент, когда player увеличивает in-flight.
    match decoder.send_packet(packet) {
        Ok(()) => {}
        Err(DecodeSendError::Backpressure(reason)) => {
            panic!("serial AUD-005 feed неожиданно получил packet backpressure: {reason:?}");
        }
        Err(DecodeSendError::Fatal(error)) => {
            panic!("AUD-005 decoder send завершился fatal: {error}");
        }
    }

    // Общий deadline ограничивает как direct completion, так и Deferred retry.
    let activity_deadline = Instant::now() + DECODER_WAIT_TIMEOUT;

    // Кадры освобождаются во время ожидания, чтобы Deferred packet мог повториться.
    let mut released_frames = 0usize;

    // Каждый следующий wait начинается с последнего уже известного epoch-а.
    let mut latest_epoch = observed_epoch;

    // Generic первый pulse не считается completion: это мог быть Deferred branch.
    loop {
        // Освобождение frame pool разрешает worker-у повторить pending packet.
        released_frames = released_frames.saturating_add(drain_and_release_frames(decoder));

        // Live snapshot закрывает окно между frame release и очередным wait.
        let current_epoch = decoder
            .decoder_activity_snapshot()
            .captured_epoch()
            .expect("production FFmpeg decoder activity не должен исчезать");

        // Два шага после исходного snapshot доказывают, что packet принят codec-ом.
        if current_epoch.get() >= completion_epoch_value {
            break;
        }

        // Оставшееся время не позволяет stale pulse перезапустить полный timeout.
        let remaining_wait = activity_deadline.saturating_duration_since(Instant::now());

        // Ждём activity строго после последнего уже обработанного epoch-а.
        let wait_outcome = activity_snapshot.wait_for_activity_after(latest_epoch, remaining_wait);

        // Новый epoch продолжает ожидание до completion threshold.
        match wait_outcome {
            VideoDecoderActivityWaitOutcome::ActivityReceived { epoch } => {
                latest_epoch = epoch;
            }
            VideoDecoderActivityWaitOutcome::NoNewActivityAfterEpoch { current_epoch, .. } => {
                latest_epoch = current_epoch;
                assert!(
                    Instant::now() < activity_deadline,
                    "worker не завершил packet после stale activity pulse",
                );
            }
            other_outcome => {
                panic!("worker не завершил packet до deadline: {other_outcome:?}");
            }
        }
    }

    // Fatal error после activity нельзя принять за корректное completion.
    if let Some(error) = decoder.try_recv_error() {
        panic!("AUD-005 worker завершил packet с fatal error: {error}");
    }

    // Completion accumulator принципиально остаётся нетронутым до terminal EOF.
    released_frames.saturating_add(drain_and_release_frames(decoder))
}

/// Доводит реальный FFmpeg DPB до terminal Drained, продолжая освобождать frame pool.
fn drain_decoder_to_terminal_eof(
    decoder: &video_backend_api::VideoBackendDecoderThreadHandle,
    generation: u64,
) -> usize {
    // EOF command приходит после доказанного completion всего packet burst-а.
    let begin_result = decoder.begin_end_of_stream_drain(generation);

    // Production worker может завершить маленький tail сразу или перейти в Draining.
    assert!(
        matches!(
            begin_result,
            VideoDecoderEndOfStreamDrainResult::Started(
                VideoDecoderEndOfStreamDrainState::Draining {
                    generation: started_generation,
                } | VideoDecoderEndOfStreamDrainState::Drained {
                    generation: started_generation,
                }
            ) if started_generation == generation
        ),
        "FFmpeg worker должен принять AUD-005 EOF drain: {begin_result:?}",
    );

    // Deadline не позволяет ошибочно назвать зависание подтверждённой потерей ACK.
    let deadline = Instant::now() + DECODER_WAIT_TIMEOUT;

    // EOF tail может публиковаться несколькими receive-side итерациями.
    let mut released_tail_frames = 0usize;

    // Ждём именно terminal decoder state, а не пустую packet queue.
    loop {
        // Освобождаем pool slots, чтобы DPB drain мог продолжаться.
        released_tail_frames =
            released_tail_frames.saturating_add(drain_and_release_frames(decoder));

        // Shared state сообщает фактический terminal outcome FFmpeg worker-а.
        match decoder.end_of_stream_drain_state() {
            VideoDecoderEndOfStreamDrainState::Drained {
                generation: drained_generation,
            } if drained_generation == generation => break,
            VideoDecoderEndOfStreamDrainState::Fatal { error, .. } => {
                panic!("AUD-005 EOF drain завершился fatal: {error}");
            }
            _ => {}
        }

        // Асинхронная fatal очередь проверяется отдельно от shared drain state.
        if let Some(error) = decoder.try_recv_error() {
            panic!("AUD-005 decoder сообщил fatal во время EOF: {error}");
        }

        // Timeout означает неподтверждённую сессию, а не accounting evidence.
        assert!(Instant::now() < deadline, "AUD-005 terminal EOF timeout");

        // Короткая пауза не создаёт busy-spin и оставляет worker-у CPU.
        thread::sleep(Duration::from_millis(1));
    }

    // Забираем последний кадр, опубликованный перед переходом в Drained.
    released_tail_frames.saturating_add(drain_and_release_frames(decoder))
}

/// Доказывает нетеряемый completion accounting после accurate seek и terminal EOF.
#[test]
#[ignore = "requires explicit generated MPEG-TS fixture and system FFmpeg libraries"]
fn accurate_seek_eof_preserves_all_packet_completions() {
    // Fixture задаётся явно и не попадает в hermetic default test suite.
    let asset_path = std::env::var_os("RUSTIPLAYER_MEDIA_PATH")
        .map(std::path::PathBuf::from)
        .expect("RUSTIPLAYER_MEDIA_PATH должен указывать на generated AUD-005 MPEG-TS");

    // Production demux выполняет seek и формирует реальный post-seek preroll burst.
    let (video_track_id, actual_seek_position, post_seek_packets) =
        collect_post_seek_video_packets(&asset_path);

    // Минимальная packet capacity воспроизводит прежнее давление на ACK path.
    let decoder = open_decoder(video_track_id);

    // Одна generation связывает post-seek packets, decoded frames и EOF drain.
    let seek_generation = 5u64;

    // Player-style accounting увеличивается только после успешного send_packet.
    let mut accepted_packets = 0usize;

    // Отдельно считаем packets, завершившиеся без немедленного output frame-а.
    let mut no_output_packets = 0usize;

    // Кадры освобождаются, но completion consumer остаётся намеренно задержанным.
    let mut released_frames_before_eof = 0usize;

    // Каждый packet проходит настоящий worker handle_packet и FFmpeg send/receive loop.
    for packet in post_seek_packets {
        // Neutral packet сохраняет фактические post-seek timestamps и payload.
        let decode_packet = decode_packet_from_demux(packet, seek_generation);

        // Completion accumulator не читается внутри helper-а ни при одном packet-е.
        let released_for_packet =
            send_and_wait_without_draining_ack(decoder.as_ref(), decode_packet);

        // Успешно принятый packet соответствует player in-flight increment.
        accepted_packets = accepted_packets.saturating_add(1);

        // Frame-threading preroll обычно завершает ранние packets без output.
        if released_for_packet == 0 {
            no_output_packets = no_output_packets.saturating_add(1);
        }

        // Frame release не затрагивает packet ACK accounting.
        released_frames_before_eof = released_frames_before_eof.saturating_add(released_for_packet);
    }

    // Burst обязан реально превысить прежнюю bounded ACK capacity.
    assert!(accepted_packets > MIN_PACKET_CHANNEL_CAPACITY);

    // Сценарий закрепляет именно быстрые no-output completions из seek preroll-а.
    assert!(
        no_output_packets > MIN_PACKET_CHANNEL_CAPACITY,
        "нужно больше no-output completions, чем прежних ACK slots: no_output={no_output_packets}, capacity={MIN_PACKET_CHANNEL_CAPACITY}",
    );

    // Реальный decoder work и DPB tail должны завершиться независимо от ACK consumer-а.
    let released_eof_tail_frames = drain_decoder_to_terminal_eof(decoder.as_ref(), seek_generation);

    // Только теперь player-facing consumer впервые забирает durable completions.
    let delivered_completions = decoder.drain_completed_packet_count();

    // Это точная модель player-core: accepted increment минус реально прочитанные ACK.
    let terminal_in_flight_packets = accepted_packets.saturating_sub(delivered_completions);

    // Каждый accepted packet обязан дать ровно один completion даже после задержки consumer-а.
    assert_eq!(
        delivered_completions, accepted_packets,
        "completion accounting потерял accepted packet: accepted={accepted_packets}, delivered={delivered_completions}",
    );

    // После terminal decoder EOF ложный VideoDecodeInFlight blocker отсутствует.
    assert_eq!(
        terminal_in_flight_packets, 0,
        "после реального decoder completion player-style in-flight должен стать нулевым",
    );

    // Повторный drain доказывает exactly-once передачу уже учтённых completions.
    assert_eq!(decoder.drain_completed_packet_count(), 0);

    // Evidence log содержит точные числа закрывающей audit session.
    eprintln!(
        "AUD005_FIXED prior_ack_capacity={MIN_PACKET_CHANNEL_CAPACITY} accepted={accepted_packets} \
         delivered_completions={delivered_completions} terminal_in_flight={terminal_in_flight_packets} \
         no_output_packets={no_output_packets} frames_before_eof={released_frames_before_eof} \
         eof_tail_frames={released_eof_tail_frames} actual_seek_us={}",
        actual_seek_position.as_micros(),
    );
}
