//! Всплеск журнала страницы из десятков тысяч однокадровых фрагментов (живая проверка
//! 09b2, Twitch VOD в Firefox: очередь сеанса переполнилась за 214 мс повтора, сеанс
//! закрылся кодом 4003, видео вернулось странице).
//!
//! Две причины, обе закрыты этой сессией:
//! - полка источника стоила квадратично на тысячах записей (замена и выбор следующего
//!   перебирали всю полку на каждый сегмент) — разбор отставал от провода;
//! - у очереди сеанса не было противодавления: единственный тормоз для провода —
//!   «полон» источника — про полку, а не про очередь, и любой разбор медленнее
//!   провода (старое железо, отладочная сборка) переполнял её.
//!
//! Что доказывает тест (настоящий процесс, безоконно): журнал в 64 МиБ из ≥ 15 000
//! однокадровых видеофрагментов и ≥ 28 000 однокадровых звуковых, стёртое начало,
//! T0 далеко от начала, всё одним всплеском на скорости провода (клиент Rust быстрее
//! любого браузера) — передача состоялась: сеанс не закрыт как нарушение протокола,
//! плеер прошёл через T0 и показал кадры от T0 до конца.
//!
//! Фрагменты строятся из первого (ключевого) кадра фикстур: один и тот же кадр с
//! новым временем декодирования, видео — с добавкой нулей в `mdat` до ~4 КиБ, чтобы
//! журнал весил как настоящий.

use super::generation_switch_clicks::{
    PageSourceBuffers, SplitMedia, is_ended, page_seek, page_seek_targets, recorded_message,
    split_fmp4,
};
use super::handoff_start::{clock_positions, playing_positions, send_burst};
use super::*;

/// Длина ролика, мкс: 15 000 кадров видео по 40 мс.
const CLIP_US: u64 = 600_000_000;
/// До этого времени страница стёрла начало (половина ролика).
const ERASED_END_US: u64 = 300_000_000;
/// T0: страница стоит далеко от начала, за 3 с до конца.
const PAGE_POSITION_US: u64 = 597_000_000;
/// Кадр видео, мкс (25 к/с).
const FRAME_US: u64 = 40_000;
/// Груз `mdat` видеофрагмента: ключевой кадр фикстуры плюс нули — так журнал весит
/// ~64 МиБ, как настоящий (решение 176).
const VIDEO_MDAT_PAYLOAD_BYTES: usize = 4_096;
/// Наименьший размер журнала, который тест обязан отправить, байт.
const MIN_JOURNAL_BYTES: usize = 60 * 1024 * 1024;
/// Наименьшее число видеофрагментов.
const MIN_VIDEO_FRAGMENTS: usize = 15_000;
/// Звук и картинка могут стартовать на шаг часов позже цели.
const START_WINDOW_US: u64 = 500_000;
/// Сколько кадров могут не показаться на старте (опоздавшие на доли кадра).
const FRAMES_LOST_AT_START: u64 = 3;

// ---- ISO BMFF: разбор первого кадра фикстуры и сборка однокадровых фрагментов ----

/// Флаги `tfhd`.
const TFHD_BASE_DATA_OFFSET: u32 = 0x1;
const TFHD_SAMPLE_DESCRIPTION_INDEX: u32 = 0x2;
const TFHD_DEFAULT_DURATION: u32 = 0x8;
const TFHD_DEFAULT_SIZE: u32 = 0x10;
const TFHD_DEFAULT_FLAGS: u32 = 0x20;
const TFHD_DEFAULT_BASE_IS_MOOF: u32 = 0x20000;
/// Флаги `trun`.
const TRUN_DATA_OFFSET: u32 = 0x1;
const TRUN_FIRST_SAMPLE_FLAGS: u32 = 0x4;
const TRUN_SAMPLE_DURATION: u32 = 0x100;
const TRUN_SAMPLE_SIZE: u32 = 0x200;
const TRUN_SAMPLE_FLAGS: u32 = 0x400;
const TRUN_SAMPLE_COMPOSITION_OFFSET: u32 = 0x800;

fn be_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().expect("u32"))
}

/// Содержимое первой коробки `kind` среди соседних коробок `bytes` (без заголовка).
fn find_box<'a>(bytes: &'a [u8], kind: &[u8; 4]) -> &'a [u8] {
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let size = be_u32(bytes, offset) as usize;
        assert!(size >= 8, "box size");
        if &bytes[offset + 4..offset + 8] == kind {
            return &bytes[offset + 8..offset + size];
        }
        offset += size;
    }
    panic!("no {} box", String::from_utf8_lossy(kind));
}

/// Шкала времени дорожки из init (`mdhd`).
fn track_timescale(init: &[u8]) -> u64 {
    let mdhd = find_box(
        find_box(find_box(find_box(init, b"moov"), b"trak"), b"mdia"),
        b"mdhd",
    );
    let timescale_at = if mdhd[0] == 0 { 12 } else { 20 };
    u64::from(be_u32(mdhd, timescale_at))
}

/// Первый кадр фрагмента фикстуры: всё, что нужно, чтобы собрать из него фрагмент.
struct FirstSample {
    track_id: u32,
    duration: u32,
    size: u32,
    flags: u32,
    composition_offset: u32,
    trun_version: u8,
    bytes: Vec<u8>,
}

/// Разбирает первый кадр фрагмента (`moof` + `mdat` с `default-base-is-moof`).
fn first_sample(fragment: &[u8]) -> FirstSample {
    let moof = find_box(fragment, b"moof");
    let traf = find_box(moof, b"traf");
    let tfhd = find_box(traf, b"tfhd");
    let tfhd_flags = be_u32(tfhd, 0) & 0x00ff_ffff;
    let track_id = be_u32(tfhd, 4);
    let mut cursor = 8;
    assert_eq!(
        tfhd_flags & TFHD_BASE_DATA_OFFSET,
        0,
        "fixture fragments address samples from their own moof"
    );
    assert_ne!(tfhd_flags & TFHD_DEFAULT_BASE_IS_MOOF, 0);
    if tfhd_flags & TFHD_SAMPLE_DESCRIPTION_INDEX != 0 {
        cursor += 4;
    }
    let mut take_default = |flag: u32| {
        (tfhd_flags & flag != 0).then(|| {
            let value = be_u32(tfhd, cursor);
            cursor += 4;
            value
        })
    };
    let default_duration = take_default(TFHD_DEFAULT_DURATION);
    let default_size = take_default(TFHD_DEFAULT_SIZE);
    let default_flags = take_default(TFHD_DEFAULT_FLAGS);

    let trun = find_box(traf, b"trun");
    let trun_version = trun[0];
    let trun_flags = be_u32(trun, 0) & 0x00ff_ffff;
    assert!(be_u32(trun, 4) >= 1, "fixture fragment has samples");
    let mut cursor = 8;
    let mut take = |flag: u32| {
        (trun_flags & flag != 0).then(|| {
            let value = be_u32(trun, cursor);
            cursor += 4;
            value
        })
    };
    let data_offset = take(TRUN_DATA_OFFSET).expect("data offset") as usize;
    let first_sample_flags = take(TRUN_FIRST_SAMPLE_FLAGS);
    let duration = take(TRUN_SAMPLE_DURATION)
        .or(default_duration)
        .expect("duration");
    let size = take(TRUN_SAMPLE_SIZE).or(default_size).expect("size");
    let flags = take(TRUN_SAMPLE_FLAGS)
        .or(first_sample_flags)
        .or(default_flags)
        .expect("flags");
    let composition_offset = take(TRUN_SAMPLE_COMPOSITION_OFFSET).unwrap_or(0);
    // Смещение — от начала `moof`, а фрагмент начинается с него.
    let bytes = fragment[data_offset..data_offset + size as usize].to_vec();
    FirstSample {
        track_id,
        duration,
        size,
        flags,
        composition_offset,
        trun_version,
        bytes,
    }
}

fn plain_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 + body.len());
    bytes.extend_from_slice(
        &u32::try_from(8 + body.len())
            .expect("box size")
            .to_be_bytes(),
    );
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(body);
    bytes
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut full = Vec::with_capacity(4 + body.len());
    full.extend_from_slice(&((u32::from(version) << 24) | (flags & 0x00ff_ffff)).to_be_bytes());
    full.extend_from_slice(body);
    plain_box(kind, &full)
}

/// `moof` с одним кадром: номер `sequence`, время декодирования `decode_time`,
/// груз `mdat` начинается через `data_offset` байт от начала `moof`.
fn one_sample_moof(
    sample: &FirstSample,
    sequence: u32,
    decode_time: u64,
    data_offset: u32,
) -> Vec<u8> {
    let mfhd = full_box(b"mfhd", 0, 0, &sequence.to_be_bytes());
    let tfhd = full_box(
        b"tfhd",
        0,
        TFHD_DEFAULT_BASE_IS_MOOF,
        &sample.track_id.to_be_bytes(),
    );
    let tfdt = full_box(b"tfdt", 1, 0, &decode_time.to_be_bytes());
    let mut trun_body = Vec::new();
    trun_body.extend_from_slice(&1_u32.to_be_bytes());
    trun_body.extend_from_slice(&data_offset.to_be_bytes());
    trun_body.extend_from_slice(&sample.duration.to_be_bytes());
    trun_body.extend_from_slice(&sample.size.to_be_bytes());
    trun_body.extend_from_slice(&sample.flags.to_be_bytes());
    trun_body.extend_from_slice(&sample.composition_offset.to_be_bytes());
    let trun = full_box(
        b"trun",
        sample.trun_version,
        TRUN_DATA_OFFSET
            | TRUN_SAMPLE_DURATION
            | TRUN_SAMPLE_SIZE
            | TRUN_SAMPLE_FLAGS
            | TRUN_SAMPLE_COMPOSITION_OFFSET,
        &trun_body,
    );
    let traf = plain_box(b"traf", &[tfhd, tfdt, trun].concat());
    plain_box(b"moof", &[mfhd, traf].concat())
}

/// Однокадровый фрагмент: `moof` + `mdat` (кадр и, если надо, нули до `mdat_payload`).
fn one_sample_fragment(
    sample: &FirstSample,
    sequence: u32,
    decode_time: u64,
    mdat_payload: Option<usize>,
) -> Vec<u8> {
    // Смещение груза зависит только от формы `moof`, не от значений: меряем по пробной.
    let probe = one_sample_moof(sample, sequence, decode_time, 0);
    let data_offset = u32::try_from(probe.len() + 8).expect("offset");
    let moof = one_sample_moof(sample, sequence, decode_time, data_offset);
    let mut payload = sample.bytes.clone();
    if let Some(padded) = mdat_payload {
        assert!(
            padded >= payload.len(),
            "the key frame fits the padded payload"
        );
        payload.resize(padded, 0);
    }
    [moof, plain_box(b"mdat", &payload)].concat()
}

/// Однокадровые фрагменты дорожки на `clip_us`: первый кадр фикстуры с новым
/// временем. Возвращает фрагменты и длительность кадра в микросекундах.
fn one_frame_journal(
    media: &SplitMedia,
    clip_us: u64,
    mdat_payload: Option<usize>,
) -> (Vec<Vec<u8>>, u64) {
    let sample = first_sample(&media.fragments[0]);
    let timescale = track_timescale(&media.init);
    let frame_us = u64::from(sample.duration) * 1_000_000 / timescale;
    let count = usize::try_from(clip_us / frame_us).expect("count");
    let fragments = (0..count)
        .map(|index| {
            let sequence = u32::try_from(index + 1).expect("sequence");
            let decode_time = u64::try_from(index).expect("index") * u64::from(sample.duration);
            one_sample_fragment(&sample, sequence, decode_time, mdat_payload)
        })
        .collect();
    (fragments, frame_us)
}

// ---- сценарий ------------------------------------------------------------

/// `SourceBuffer.remove(0, ERASED_END_US)` полосы.
fn erasure(source_buffer: u16) -> Message {
    Message::text(format!(
        r#"{{"type":"source_buffer_removed","generation":1,"source_buffer":{source_buffer},"start_us":0,"end":{{"kind":"at","time_us":{ERASED_END_US}}}}}"#
    ))
}

fn page_plays() -> Message {
    Message::text(r#"{"type":"page_playback","request":{"kind":"play"}}"#)
}

/// Журнал страницы: T0, init, стёртая история, стирание, удержанная часть, конец.
/// Возвращает сообщения и число байтов груза.
fn replayed_journal() -> (Vec<Message>, usize) {
    let audio = split_fmp4("audio-6s.mp4");
    let video = split_fmp4("video-64-6s.mp4");
    let (audio_fragments, audio_frame_us) = one_frame_journal(&audio, CLIP_US, None);
    let (video_fragments, video_frame_us) =
        one_frame_journal(&video, CLIP_US, Some(VIDEO_MDAT_PAYLOAD_BYTES));
    assert_eq!(video_frame_us, FRAME_US);
    assert!(
        video_fragments.len() >= MIN_VIDEO_FRAGMENTS,
        "{}",
        video_fragments.len()
    );
    let erased_audio = usize::try_from(ERASED_END_US / audio_frame_us).expect("count");
    let erased_video = usize::try_from(ERASED_END_US / video_frame_us).expect("count");

    let mut page = PageSourceBuffers::new(1);
    let mut journal_bytes = 0;
    let mut messages: Vec<Message> = ["001.json", "003.json", "004.json"]
        .into_iter()
        .map(recorded_message)
        .collect();
    messages.push(Message::text(format!(
        r#"{{"type":"duration_changed","generation":1,"duration":{{"kind":"finite","duration_us":{CLIP_US}}}}}"#
    )));
    messages.push(page_seek(PAGE_POSITION_US));
    messages.push(page.frame(0, &audio.init));
    messages.push(page.frame(1, &video.init));
    // Стёртая история: как у Twitch, начинается не с нуля.
    let mut push_fragments =
        |messages: &mut Vec<Message>, source_buffer: u16, fragments: &[Vec<u8>]| {
            for fragment in fragments {
                journal_bytes += fragment.len();
                messages.push(page.frame(source_buffer, fragment));
            }
        };
    push_fragments(&mut messages, 0, &audio_fragments[1..erased_audio]);
    push_fragments(&mut messages, 1, &video_fragments[1..erased_video]);
    messages.push(erasure(0));
    messages.push(erasure(1));
    // Удержанное страницей: вперемежку, как кладёт плеер страницы.
    let held_audio = &audio_fragments[erased_audio..];
    let held_video = &video_fragments[erased_video..];
    let audio_per_video = held_audio.len().div_ceil(held_video.len());
    let mut audio_next = 0;
    for video_fragment in held_video {
        let audio_end = (audio_next + audio_per_video).min(held_audio.len());
        push_fragments(&mut messages, 0, &held_audio[audio_next..audio_end]);
        audio_next = audio_end;
        push_fragments(&mut messages, 1, std::slice::from_ref(video_fragment));
    }
    push_fragments(&mut messages, 0, &held_audio[audio_next..]);
    messages.push(Message::text(r#"{"type":"end_of_stream","generation":1}"#));
    messages.push(page_plays());
    (messages, journal_bytes)
}

#[test]
fn a_journal_of_tens_of_thousands_of_one_frame_fragments_hands_off_at_wire_speed() {
    let Some(process) = SessionProcessUnderTest::start("journal-burst", VideoMode::Headless) else {
        return;
    };
    let mut socket = process.open_attached_session();
    let (messages, journal_bytes) = replayed_journal();
    assert!(
        journal_bytes >= MIN_JOURNAL_BYTES,
        "journal is {journal_bytes} bytes"
    );
    let items = messages.len();
    let started = Instant::now();
    send_burst(&mut socket, messages);
    let burst = started.elapsed();
    println!("journal: {items} items, {journal_bytes} bytes, sent in {burst:?}");

    let messages = read_player_messages_until(&mut socket, "the clock state ended", is_ended);
    assert_eq!(
        page_seek_targets(&messages),
        Vec::<u64>::new(),
        "the page must stay at its own position: {messages:?}"
    );
    let playing = playing_positions(&messages);
    assert!(
        playing
            .iter()
            .any(|clock| (PAGE_POSITION_US..PAGE_POSITION_US + START_WINDOW_US).contains(clock)),
        "the player went through the page position: {playing:?}"
    );
    // Ни одного тика до T0: T0 за 3 с до конца, демуксер доходит до конца потока раньше
    // коммита перемотки, и плеер садится на цель в `Draining`. Тень верит любому тику и
    // прыгнула бы в 0.
    let clocks = clock_positions(&messages);
    assert!(
        clocks
            .iter()
            .all(|clock| *clock + FRAME_US >= PAGE_POSITION_US),
        "the page heard a position before its own: {clocks:?}"
    );

    send_tab_gone_and_expect_close(&mut socket);
    // Сеанс не закрывался как нарушение протокола и очередь не переполнялась.
    let summary = process.bus.wait_for_log_line_without(
        "Browser playback summary kind=final",
        &[
            "protocol violation",
            "queue overflowed",
            "ProtocolViolation",
        ],
    );
    let frames_from_position = (CLIP_US - PAGE_POSITION_US) / FRAME_US;
    assert!(
        log_number(&summary, "presented_frames") + FRAMES_LOST_AT_START >= frames_from_position,
        "every frame from the page position reaches the screen: {summary}"
    );
}
