//! Конец показа, когда источник обрезал данные раньше конца медиа (решение 133).
//!
//! Браузерная вкладка ушла посреди скачивания: плеер доигрывает только полученное.
//! Раньше на `Ended` позиция всегда прыгала в длительность фильма — ползунок улетал в
//! конец, хотя досмотрено было меньше, — а звук, прочитанный впрок до обрыва, играл
//! дальше застывшей картинки. Теперь demuxer говорит `DemuxDataEnd::CutShort`:
//! - с известным концом доступного — показ встаёт на нём (звук впрок не звучит),
//!   позиция = этот конец;
//! - без него — позиция там, где встал показ.
//!
//! У настоящего конца (`Complete`) — как было.

use super::test_support::*;
use super::*;

use media_core::DemuxDataEnd;

/// Длительность медиа, которую заявил источник.
const MEDIA_DURATION: Duration = Duration::from_secs(30);
/// Где кончилось полученное: досмотр встаёт здесь.
const RECEIVED_END: Duration = Duration::from_secs(12);
/// Сколько звука плеер прочитал впрок за концом доступного.
const AUDIO_READ_PAST_END: Duration = Duration::from_secs(3);

/// Fake demuxer с заданным ответом шва «каким был конец данных».
struct DataEndDemuxer {
    /// Обычный fake источник (дорожки, длительность, пустой поток пакетов).
    inner: FakeDemuxer,
    /// Что источник говорит о конце своих данных.
    data_end: DemuxDataEnd,
}

impl Demuxer for DataEndDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        self.inner.tracks()
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    fn data_end(&self) -> DemuxDataEnd {
        self.data_end
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        self.inner.next_event()
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<media_core::DemuxSeekResult> {
        self.inner.seek(timestamp)
    }
}

/// Видео + звук на позиции 0 (часы звука идут от нуля): декодер дожат, выход звука
/// держит `buffered_audio_ms` записанного впрок. Источник отвечает `data_end` на вопрос
/// о конце данных.
fn session_with_data_end(
    data_end: DemuxDataEnd,
    buffered_audio_ms: f64,
) -> (PlayerSession, ScriptedAudioOutputHandle) {
    let mut session = PlayerSession::new();
    let tracks = vec![
        fake_track(1, TrackKind::Video),
        fake_track(2, TrackKind::Audio),
    ];
    let demuxer = DataEndDemuxer {
        inner: FakeDemuxer::new(
            tracks.clone(),
            Some(MEDIA_DURATION),
            Arc::new(Mutex::new(Vec::new())),
        ),
        data_end,
    };
    session
        .pipeline
        .install_opened_media(Box::new(demuxer), None, None, tracks.clone());
    session
        .select_default_video_track(&tracks, "fake media lacking video track")
        .expect("fake video track gets a decode requirement");
    session
        .pipeline
        .select_audio_track(media_core::TrackId::new(2));
    session.set_snapshot_duration(Some(MEDIA_DURATION));
    session.set_playback_state(PlaybackState::Paused);

    let decoder = SharedFakeVideoDecoderThread::new();
    session.pipeline.set_video_decoder_thread(decoder.clone());
    decoder.set_eof_drain_state(video_core::VideoDecoderEndOfStreamDrainState::Drained {
        generation: session.pipeline.seek_generation(),
    });
    let audio_output = install_ready_audio_runtime(&mut session, buffered_audio_ms, None);
    session.pipeline.reset_clocks_for_seek(Duration::ZERO);
    (session, audio_output)
}

/// Часы звука показывают `position`.
fn set_audio_clock(audio_output: &ScriptedAudioOutputHandle, position: Duration) {
    *audio_output.clock.now.lock().expect("scripted clock") = position;
}

/// Видео + звук, доигранные до EOF на позиции [`RECEIVED_END`]: звук хвоста доигран.
fn drained_session_with_data_end(data_end: DemuxDataEnd) -> PlayerSession {
    // Пустой output buffer: звук хвоста уже доигран.
    let (mut session, audio_output) = session_with_data_end(data_end, 0.0);
    set_audio_clock(&audio_output, RECEIVED_END);
    session.update_current_position(RECEIVED_END);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.enter_eof_drain();
    session
}

/// Звук прочитан впрок на [`AUDIO_READ_PAST_END`] дальше конца доступного: пакеты ждут
/// декода, выход держит записанный PCM. Сессия играет.
fn playing_session_with_audio_read_past_the_end(
    data_end: DemuxDataEnd,
) -> (PlayerSession, ScriptedAudioOutputHandle) {
    let buffered_ms = AUDIO_READ_PAST_END.as_secs_f64() * 1000.0;
    let (mut session, audio_output) = session_with_data_end(data_end, buffered_ms);
    let generation = session.pipeline.seek_generation();
    let packet_step = Duration::from_millis(500);
    let mut packet_pts = RECEIVED_END;
    while packet_pts < RECEIVED_END + AUDIO_READ_PAST_END {
        session
            .pipeline
            .enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
                media_core::TrackId::new(2),
                packet_pts,
                None,
                None,
                generation,
                Bytes::from_static(b"audio-packet"),
            ));
        packet_pts += packet_step;
    }
    session.set_playback_state(PlaybackState::Playing);
    (session, audio_output)
}

/// Один tick в момент `now`.
fn tick_at(session: &mut PlayerSession, now: Instant) {
    let _tick_result = session.tick(PlayerTickContext::new(now));
}

/// Несколько обычных tick-ов EOF drain-а.
fn tick_drain(session: &mut PlayerSession) {
    let started_at = Instant::now();
    for tick_index in 0..3 {
        tick_at(session, started_at + Duration::from_millis(tick_index));
    }
}

/// Конец доступного 12 с, звук прочитан до 15 с: показ встаёт на 12 с — звук впрок не
/// звучит (выход на паузе, ждущие пакеты выброшены), позиция ровно 12 с и не идёт дальше.
#[test]
fn audio_read_past_the_playable_end_stops_there() {
    let (mut session, audio_output) =
        playing_session_with_audio_read_past_the_end(DemuxDataEnd::CutShort {
            playable_end: Some(RECEIVED_END),
        });
    let now = Instant::now();

    // До конца доступного показ идёт.
    set_audio_clock(&audio_output, RECEIVED_END - Duration::from_millis(100));
    tick_at(&mut session, now);
    assert_ne!(session.playback_state(), PlaybackState::Ended);
    let pauses_before_end = audio_output.pause_count();

    set_audio_clock(&audio_output, RECEIVED_END);
    tick_at(&mut session, now + Duration::from_millis(100));

    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert_eq!(session.snapshot().current_position, RECEIVED_END);
    assert!(
        audio_output.pause_count() > pauses_before_end,
        "the audio read past the end must not keep sounding"
    );
    assert_eq!(
        session.pipeline.pending_audio_packet_len(),
        0,
        "audio packets past the end are dropped"
    );

    // Часы выхода (будь он не на паузе) ушли бы дальше — позиция стоит на конце.
    set_audio_clock(&audio_output, RECEIVED_END + AUDIO_READ_PAST_END);
    tick_at(&mut session, now + Duration::from_millis(200));
    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert_eq!(session.snapshot().current_position, RECEIVED_END);
}

/// Настоящий конец (`Complete`) в той же точке ничего не останавливает: звук, прочитанный
/// дальше, играется, как у любого файла.
#[test]
fn complete_data_keeps_playing_past_the_same_point() {
    let (mut session, audio_output) =
        playing_session_with_audio_read_past_the_end(DemuxDataEnd::Complete);

    set_audio_clock(&audio_output, RECEIVED_END + Duration::from_millis(100));
    tick_at(&mut session, Instant::now());

    assert_ne!(session.playback_state(), PlaybackState::Ended);
    assert!(session.pipeline.pending_audio_packet_len() > 0);
}

/// Кадров впереди нет, а звук впрок играет из выхода (EOF уже прочитан, ждущих пакетов
/// нет): дедлайн конца доступного — ровно когда часы показа дойдут до него, и план
/// worker-а не позже него. У настоящего конца такого дедлайна нет.
#[test]
fn the_worker_wakes_at_the_playable_end() {
    let buffered_ms = AUDIO_READ_PAST_END.as_secs_f64() * 1000.0;
    let session_at_100_ms_before_the_end = |data_end| {
        let (mut session, audio_output) = session_with_data_end(data_end, buffered_ms);
        session.set_playback_state(PlaybackState::Playing);
        session.enter_eof_drain();
        set_audio_clock(&audio_output, RECEIVED_END - Duration::from_millis(100));
        session
    };
    let cut_short = session_at_100_ms_before_the_end(DemuxDataEnd::CutShort {
        playable_end: Some(RECEIVED_END),
    });
    let now = Instant::now();

    let end_delay = cut_short
        .cut_short_playable_end_wall_delay(now)
        .expect("a deadline at the playable end");
    assert!(
        end_delay.abs_diff(Duration::from_millis(100)) <= Duration::from_millis(2),
        "the deadline is when the presentation reaches the playable end: {end_delay:?}"
    );
    let plan = cut_short.worker_wakeup_plan(
        now,
        &PlayerTickConfig::default(),
        Duration::from_millis(5),
        Duration::from_millis(250),
    );
    assert!(plan.delay.expect("a deadline") <= end_delay);

    let complete = session_at_100_ms_before_the_end(DemuxDataEnd::Complete);
    assert_eq!(complete.cut_short_playable_end_wall_delay(now), None);
}

/// Без известного конца доступного (файл, обрезанный посреди байтов): позиция там, где
/// встал показ, а не длительность.
#[test]
fn cut_short_data_without_a_known_end_keeps_the_ended_position_where_presentation_stopped() {
    let mut session = drained_session_with_data_end(DemuxDataEnd::CutShort { playable_end: None });

    tick_drain(&mut session);

    assert_eq!(session.playback_state(), PlaybackState::Ended);
    let position = session.snapshot().current_position;
    assert!(
        position < MEDIA_DURATION,
        "cut-short data must not jump the position to the media duration: {position:?}"
    );
    assert!(
        position.abs_diff(RECEIVED_END) <= Duration::from_millis(50),
        "the position stays where the received data ended: {position:?}"
    );
}

#[test]
fn complete_data_still_ends_at_the_media_duration() {
    let mut session = drained_session_with_data_end(DemuxDataEnd::Complete);

    tick_drain(&mut session);

    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert_eq!(session.snapshot().current_position, MEDIA_DURATION);
}
