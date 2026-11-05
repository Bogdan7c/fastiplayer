//! Мусор вместо контейнера «до кадра» (мост, сессия 09c, инвариант 1 README).
//!
//! Живая находка (страница `attack-garbage` стенда): настоящий fMP4 играет, потом страница
//! кладёт в `SourceBuffer` 64 КиБ случайных байтов. Сканер читал случайный заголовок как
//! незнакомый бокс длиной в гигабайты и «пропускал» его: ни ошибки, ни конца, сеанс
//! кончался только с уходом вкладки. Браузерный разборщик MSE на этом месте падает.
//!
//! Здесь — обратная сторона того же правила, и она важнее: настоящие служебные боксы и
//! элементы, которые браузеры принимают, обязаны доигрываться до последнего кадра.
//! Ложный отказ ломает настоящие сайты, а это хуже самого дефекта.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::time::Duration;

use player_core::PlaybackState;
use pushed_media_source::{
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamError,
    PushedStreamFailureClass, PushedStreamOpener, PushedStreamWriter, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture, pseudo_random_bytes};
use support::playback::{PlaybackReport, TickObservation, open, play, play_observed};

const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);
/// Столько мусора кладёт страница стенда `attack-garbage` за один раз.
const GARBAGE_BYTES: usize = 64 * 1024;
/// fMP4-фикстуры: 10 фрагментов по 0,4 с, 25 к/с.
const MP4_FRAGMENT_COUNT: usize = 10;
const MP4_FRAME_COUNT: usize = 100;
/// До этого места мёртвый поток не доигрывается: весь ролик (4 с) лежит на полке.
const MP4_NOT_PLAYED_OUT_BEFORE: Duration = Duration::from_millis(3_200);
/// WebM `video-vp9-64-4fps.webm`: 4 к/с, 4 с.
const WEBM_FRAME_COUNT: usize = 16;
/// Страница кладёт мусор, когда плеер уже показал что-то настоящее.
const GARBAGE_AT_POSITION: Duration = Duration::from_millis(800);
/// `Cluster`: ID WebM в файле.
const CLUSTER_ID: [u8; 4] = [0x1F, 0x43, 0xB6, 0x75];
/// Служебные боксы, которые принимает хотя бы один браузер.
const MP4_SERVICE_BOXES: [&[u8; 4]; 14] = [
    b"free", b"skip", b"styp", b"sidx", b"emsg", b"prft", b"mfra", b"uuid", b"pdin", b"bloc",
    b"meta", b"meco", b"ssix", b"pssh",
];
/// ID служебных элементов WebM: `SeekHead`, `Cues`, `Tags`, `Chapters`, `Attachments`,
/// `Void`, `CRC-32`.
const WEBM_SERVICE_IDS: [&[u8]; 7] = [
    &[0x11, 0x4D, 0x9B, 0x74],
    &[0x1C, 0x53, 0xBB, 0x6B],
    &[0x12, 0x54, 0xC3, 0x67],
    &[0x10, 0x43, 0xA7, 0x70],
    &[0x19, 0x41, 0xA4, 0x69],
    &[0xEC],
    &[0xBF],
];

fn policy() -> PushedSourcePolicy {
    PushedSourcePolicy::recommended().expect("policy")
}

/// Поток из одной полосы.
fn single_lane_stream(container: PushedContainer) -> (PushedStreamWriter, PushedStreamOpener) {
    let (mut writer, opener) = pushed_stream(policy()).expect("stream");
    writer.add_lane(VIDEO, container).expect("lane");
    (writer, opener)
}

/// Начала кластеров WebM: поиск по ID, тела кластеров фикстур этих байтов не содержат
/// (проверяет `play_and_throw_garbage`: кадры настоящих кластеров доходят до показа).
fn cluster_offsets(file: &[u8]) -> Vec<usize> {
    file.windows(CLUSTER_ID.len())
        .enumerate()
        .filter(|(_, window)| *window == CLUSTER_ID)
        .map(|(position, _)| position)
        .collect()
}

/// Играет принятое и, когда плеер дошёл до [`GARBAGE_AT_POSITION`], кладёт мусор дважды,
/// как страница стенда (`lane` — полоса мусора). Проверяет ответ потока на оба куска.
fn play_and_throw_garbage(
    mut writer: PushedStreamWriter,
    opener: &mut PushedStreamOpener,
    lane: LaneId,
    garbage_at: Duration,
) -> (PlaybackReport, PushedStreamWriter, PushedStreamError) {
    let mut failure = None;
    let report = play_observed(open(opener), |observation: TickObservation| {
        if failure.is_some() || observation.position < garbage_at {
            return;
        }
        let error = writer
            .append(lane, &pseudo_random_bytes(1, GARBAGE_BYTES))
            .expect_err("garbage is refused at once, not waited for");
        // Второй кусок мусора: поток уже мёртв, причина — первая.
        assert_eq!(
            writer.append(lane, &pseudo_random_bytes(2, GARBAGE_BYTES)),
            Err(error.clone())
        );
        failure = Some(error);
    });
    let failure = failure.expect("the player reached the garbage position");
    (report, writer, failure)
}

/// Честный исход: разбор байтов этой полосы, класс «не разбирается» (именно он
/// становится `stream_rejected{unparsable_media}`), плеер встал с ошибкой, а не ждёт.
fn assert_honest_rejection(
    report: &PlaybackReport,
    writer: &PushedStreamWriter,
    failure: &PushedStreamError,
    lane: LaneId,
) {
    assert!(
        matches!(failure, PushedStreamError::Framing { lane: failed, .. } if *failed == lane),
        "{failure:?}"
    );
    assert_eq!(
        failure.failure_class(),
        PushedStreamFailureClass::UnparsableMedia
    );
    assert_eq!(writer.failure().as_ref(), Some(failure));
    assert_eq!(
        report.final_state,
        PlaybackState::Failed,
        "the player must stop with an error, not wait for data: {:?}",
        report.state_trace.last()
    );
    assert!(
        !report.errors.is_empty(),
        "the player must report the failure"
    );
    assert!(
        !report.frames.is_empty(),
        "valid segments reached the screen before the garbage"
    );
}

/// fMP4 из двух полос, как у страницы стенда: настоящее играет, мусор кончает поток.
#[test]
fn garbage_after_playing_fmp4_ends_the_stream_with_a_parse_failure() {
    let video = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
    let audio = FragmentedMp4::split(&own_fixture("audio-aac-4s.mp4"));
    let (mut writer, mut opener) = pushed_stream(policy()).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    writer.set_duration(PushedDuration::Finite(Duration::from_secs(4)));
    writer
        .append(VIDEO, &video.stream_of(0..MP4_FRAGMENT_COUNT))
        .expect("video");
    writer
        .append(AUDIO, &audio.stream_of(0..MP4_FRAGMENT_COUNT))
        .expect("audio");

    let (report, writer, failure) =
        play_and_throw_garbage(writer, &mut opener, VIDEO, GARBAGE_AT_POSITION);

    assert_honest_rejection(&report, &writer, &failure, VIDEO);
    let last_shown = report.frames.last().expect("frames").pts;
    assert!(
        last_shown >= GARBAGE_AT_POSITION - Duration::from_millis(200),
        "the video played up to the garbage: {last_shown:?}"
    );
    // Поток мёртв, как в браузере после ошибки разбора: полка не доигрывается. Показать
    // успевает только то, что плеер уже прочитал впрок (~1,5 с), — и потом ошибка.
    assert!(
        last_shown < MP4_NOT_PLAYED_OUT_BEFORE,
        "a dead stream does not play the shelf out: {last_shown:?}"
    );
}

/// WebM с кластерами известного размера: мусор приходит между кластерами.
#[test]
fn garbage_after_playing_webm_clusters_ends_the_stream_with_a_parse_failure() {
    let file = own_fixture("video-vp9-64-4fps.webm");
    let clusters = cluster_offsets(&file);
    assert!(clusters.len() >= 3, "{clusters:?}");
    let (mut writer, mut opener) = single_lane_stream(PushedContainer::WebM);
    writer.set_duration(PushedDuration::Finite(Duration::from_secs(4)));
    writer
        .append(VIDEO, &file[..clusters[3]])
        .expect("clusters");

    let (report, writer, failure) =
        play_and_throw_garbage(writer, &mut opener, VIDEO, GARBAGE_AT_POSITION);

    assert_honest_rejection(&report, &writer, &failure, VIDEO);
}

/// WebM «вживую»: кластер неизвестного размера ещё открыт, мусор приходит в него.
#[test]
fn garbage_inside_an_open_webm_cluster_ends_the_stream_with_a_parse_failure() {
    let file = own_fixture("file-vp9-opus-live.webm");
    let clusters = cluster_offsets(&file);
    assert!(clusters.len() >= 3, "{clusters:?}");
    let (mut writer, mut opener) = single_lane_stream(PushedContainer::WebM);
    writer.set_duration(PushedDuration::Unknown);
    // Последний переданный кластер открыт: он кончится только со следующим элементом.
    writer
        .append(VIDEO, &file[..clusters[clusters.len() - 1]])
        .expect("clusters");

    let (report, writer, failure) =
        play_and_throw_garbage(writer, &mut opener, VIDEO, Duration::from_millis(400));

    assert_honest_rejection(&report, &writer, &failure, VIDEO);
}

/// Мусор на первых байтах: поток не открывается, а не ждёт начала вечно. Режем мусор
/// кусками: заголовок, разорванный между кусками, ничего не меняет.
#[test]
fn garbage_on_the_first_bytes_never_opens_in_any_container() {
    let garbage = pseudo_random_bytes(3, GARBAGE_BYTES);
    for container in [
        PushedContainer::FragmentedMp4,
        PushedContainer::WebM,
        PushedContainer::MpegTs,
    ] {
        let (mut writer, mut opener) = single_lane_stream(container);
        let error = garbage
            .chunks(5)
            .find_map(|chunk| writer.append(VIDEO, chunk).err())
            .unwrap_or_else(|| panic!("{container:?}: the garbage was swallowed"));
        assert_eq!(
            error.failure_class(),
            PushedStreamFailureClass::UnparsableMedia,
            "{container:?}: {error:?}"
        );
        assert!(
            matches!(opener.poll_open(), Err(failure) if failure == error),
            "{container:?}: the player must learn the same reason"
        );
    }
}

/// Страница объявляет бокс на 3 ГиБ после настоящего сегмента и капает настоящие байты:
/// отказ по заголовку, а не вечное «данные идут».
#[test]
fn a_declared_three_gigabyte_service_box_is_refused_without_holding_it() {
    let video = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
    let (mut writer, _opener) = single_lane_stream(PushedContainer::FragmentedMp4);
    writer.append(VIDEO, &video.stream_of(0..1)).expect("valid");
    let held_before = writer.held_bytes();
    let mut header = (3_u32 * 1024 * 1024 * 1024).to_be_bytes().to_vec();
    header.extend_from_slice(b"free");

    let error = writer.append(VIDEO, &header).expect_err("over any segment");

    assert!(
        matches!(error, PushedStreamError::Framing { lane: VIDEO, .. }),
        "{error:?}"
    );
    assert_eq!(
        error.failure_class(),
        PushedStreamFailureClass::UnparsableMedia
    );
    assert!(writer.held_bytes() <= held_before + header.len());
}

/// Бокс с настоящим типом и телом: `[размер][тип][тело]`.
fn mp4_box(box_type: &[u8; 4], body_len: usize) -> Vec<u8> {
    let mut bytes = u32::try_from(8 + body_len)
        .expect("small box")
        .to_be_bytes()
        .to_vec();
    bytes.extend_from_slice(box_type);
    bytes.extend(std::iter::repeat_n(0, body_len));
    bytes
}

/// Страхование от ложного отказа: каждый служебный бокс в потоке, каждый кадр на экране.
#[test]
fn every_service_box_a_browser_accepts_plays_every_frame() {
    let video = FragmentedMp4::split(&own_fixture("video-h264-64-4s.mp4"));
    let mut lane_bytes = mp4_box(b"styp", 16);
    lane_bytes.extend_from_slice(&video.initialization);
    for (index, service_type) in MP4_SERVICE_BOXES.iter().enumerate() {
        lane_bytes.extend(mp4_box(service_type, 32));
        let fragment = &video.fragments[index % MP4_FRAGMENT_COUNT];
        // Боксов больше, чем фрагментов: лишние идут подряд, как `styp`+`sidx`+`prft`.
        if index < MP4_FRAGMENT_COUNT {
            lane_bytes.extend_from_slice(fragment);
        }
    }
    let (mut writer, mut opener) = single_lane_stream(PushedContainer::FragmentedMp4);
    writer.set_duration(PushedDuration::Finite(Duration::from_secs(4)));
    // Куски не совпадают с боксами, как у настоящих плееров.
    for chunk in lane_bytes.chunks(977) {
        writer.append(VIDEO, chunk).expect("legitimate boxes");
    }
    writer.end_of_stream().expect("end of stream");

    let report = play(open(&mut opener), |_| {});

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.final_state, PlaybackState::Ended);
    assert_eq!(report.frames.len(), MP4_FRAME_COUNT);
}

/// Служебный элемент WebM с 8-байтовым размером.
fn webm_element(id: &[u8], body_len: u64) -> Vec<u8> {
    let mut bytes = id.to_vec();
    bytes.push(0x01);
    bytes.extend_from_slice(&body_len.to_be_bytes()[1..]);
    bytes.extend(std::iter::repeat_n(
        0,
        usize::try_from(body_len).expect("small element"),
    ));
    bytes
}

/// Страхование от ложного отказа: служебные элементы между кластерами WebM.
#[test]
fn every_service_element_of_webm_plays_every_frame() {
    let file = own_fixture("video-vp9-64-4fps.webm");
    let clusters = cluster_offsets(&file);
    let mut lane_bytes = file[..clusters[0]].to_vec();
    let mut service_elements = WEBM_SERVICE_IDS.iter().cycle();
    let cluster_ends = clusters[1..].iter().copied().chain([file.len()]);
    for (start, end) in clusters.iter().copied().zip(cluster_ends) {
        lane_bytes.extend(webm_element(service_elements.next().expect("cycle"), 4));
        lane_bytes.extend_from_slice(&file[start..end]);
        lane_bytes.extend(webm_element(service_elements.next().expect("cycle"), 4));
    }
    let (mut writer, mut opener) = single_lane_stream(PushedContainer::WebM);
    writer.set_duration(PushedDuration::Finite(Duration::from_secs(4)));
    for chunk in lane_bytes.chunks(611) {
        writer.append(VIDEO, chunk).expect("legitimate elements");
    }
    writer.end_of_stream().expect("end of stream");

    let report = play(open(&mut opener), |_| {});

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.final_state, PlaybackState::Ended);
    assert_eq!(report.frames.len(), WEBM_FRAME_COUNT);
}
