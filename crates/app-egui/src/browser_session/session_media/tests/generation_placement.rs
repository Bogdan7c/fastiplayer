//! Ворота нового поколения (04d, путь C, решения 146–147) сквозь медиа-часть: плеер и
//! страница подделаны, поток поколения — настоящий `pushed-media-source`.
//!
//! Сценарий живой находки: поколение 1 играет, страница открывает поколение 2 (смена
//! качества), зритель кликает туда, где у поколения 2 данных нет. Раньше поколение
//! уходило плееру сразу и вставало без данных навсегда (`VideoPreflightTimeout`).
//!
//! Фикстура протокола: `video.mp4` (init 733 байта, фрагменты 0 и 0,2 с) и `audio.mp4`
//! (звук 0,4 с). У поколения 2 сначала есть только начало ролика (0–0,4 с); байты у цели
//! — тот же ролик со смещением времени (`timestampOffset`), как YouTube кладёт коробки
//! с позиции страницы.

use fastiplayer_config::PlayerSeekConfig;
use player_core::PreparedInitialPosition;
use pushed_media_source::TimestampOffset;

use super::*;
use crate::browser_session::session_media::generation_placement::GenerationWait;
use crate::viewer_transport::ViewerTransportIntent;

/// Конец init-сегмента видео фикстуры.
pub(super) const VIDEO_INIT_END: usize = 733;
/// Длительность поколения 2 (ролик с YouTube: минуты, а данных пока — начало).
pub(super) const LONG_CLIP: Duration = Duration::from_secs(600);
/// Сколько крутить цикл, чтобы убедиться, что поколение НЕ отдано плееру: потоку
/// хватает долей секунды, чтобы открыться (см. `drive_until_loaded`).
pub(super) const HOLD_CHECK: Duration = Duration::from_millis(600);

/// Конец init-сегмента fMP4 (`ftyp` + `moov`): верхние коробки до первой `moof`.
pub(super) fn init_end(file: &[u8]) -> usize {
    let mut offset = 0;
    while offset + 8 <= file.len() {
        let size = u32::from_be_bytes(file[offset..offset + 4].try_into().expect("size")) as usize;
        if &file[offset + 4..offset + 8] == b"moof" {
            return offset;
        }
        offset += size;
    }
    file.len()
}

/// Поколение с данными только в начале ролика (0–0,4 с) и длительностью `duration`;
/// конца потока нет — страница ещё качает.
pub(super) fn generation_with_head(
    session: &SessionRef,
    number: u32,
    duration: PushedDuration,
) -> (PushedStreamWriter, IntakeNotice) {
    let (mut writer, notice) = stream_generation(session, number, false);
    writer.set_duration(duration);
    for (lane, file) in [(0, "video.mp4"), (1, "audio.mp4")] {
        let lane = LaneId::new(lane);
        writer
            .add_lane(lane, PushedContainer::FragmentedMp4)
            .expect("lane");
        writer.append(lane, &fixture_media(file)).expect("append");
    }
    (writer, notice)
}

/// Страница положила байты у `at`: тот же ролик со смещением времени (без init).
pub(super) fn append_media_at(writer: &mut PushedStreamWriter, at: Duration) {
    let offset = TimestampOffset::from_micros(i64::try_from(at.as_micros()).expect("micros"));
    let audio = fixture_media("audio.mp4");
    let video = fixture_media("video.mp4");
    for (lane, media) in [
        (0, &video[VIDEO_INIT_END..]),
        (1, &audio[init_end(&audio)..]),
    ] {
        let lane = LaneId::new(lane);
        writer.set_timestamp_offset(lane, offset).expect("offset");
        writer.append(lane, media).expect("append");
    }
}

pub(super) fn page_time(position: Duration) -> MediaTime {
    MediaTime::from_micros(u64::try_from(position.as_micros()).expect("micros")).expect("time")
}

/// Цели `seek`, о которых попросили страницу.
pub(super) fn page_seek_targets(messages: &[PlayerSessionMessage]) -> Vec<Duration> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Seek { target_us, .. } => {
                Some(Duration::from_micros(target_us.as_micros()))
            }
            _ => None,
        })
        .collect()
}

/// Перемотки, которые получил плеер.
pub(super) fn player_seeks(calls: &[PlayerCall]) -> Vec<Duration> {
    calls
        .iter()
        .filter_map(|call| match call {
            PlayerCall::Seek(target) => Some(*target),
            _ => None,
        })
        .collect()
}

/// Где стояло медиа `index`-й загрузки (`None` — с начала).
pub(super) fn start_of_load(harness: &Harness, index: usize) -> Option<Duration> {
    match harness.player.loaded_media.borrow()[index].initial_position {
        PreparedInitialPosition::Beginning => None,
        PreparedInitialPosition::PositionedAt {
            target_position, ..
        } => Some(target_position.as_duration()),
    }
}

impl Harness {
    /// Крутит цикл в настоящем времени `period`; возвращает команды плееру. Всё, что
    /// ушло странице, остаётся в подделке страницы.
    pub(super) fn drive_for(&mut self, period: Duration) -> Vec<PlayerCall> {
        let until = Instant::now() + period;
        let mut calls = Vec::new();
        while Instant::now() < until {
            self.media.drive(Instant::now());
            calls.extend(self.player.take_calls());
            std::thread::sleep(Duration::from_millis(5));
        }
        calls
    }

    /// Сколько загрузок получил плеер.
    pub(super) fn load_count(&self) -> usize {
        self.player.loaded_media.borrow().len()
    }

    /// Плеер «поставил» последнюю загрузку и играет её с `position`.
    pub(super) fn show_last_load(&mut self, position: Duration) {
        let ticket = self.player.load_ticket(self.load_count() - 1);
        self.player
            .show_load(ticket, position, PlaybackState::Playing);
        self.media.drive(Instant::now());
    }
}

/// Та же медиа-часть с другим сроком ожидания данных у цели (до появления поколения:
/// ждущее поколение берёт срок в момент, когда страница его открыла).
pub(super) fn with_wait(harness: Harness, wait: Duration) -> Harness {
    let Harness {
        media,
        player,
        page,
        closer,
    } = harness;
    Harness {
        media: media.with_placement_policy(GenerationPlacementPolicy::new(wait)),
        player,
        page,
        closer,
    }
}

/// Долгий срок для тестов правил ожидания: запасной старт после срока не может
/// замаскировать ошибку проверяемого правила даже на медленной машине.
pub(super) const LONG_WAIT: Duration = Duration::from_secs(3_600);

/// Поколение 1 играет, поколение 2 (данные 0–0,4 с) открывается; писатели держит тест.
/// Срок ожидания данных у цели — [`LONG_WAIT`].
pub(super) fn second_generation_opening(
    duration: PushedDuration,
) -> (Harness, SessionRef, PushedStreamWriter, PushedStreamWriter) {
    second_generation_opening_with(duration, LONG_WAIT)
}

/// То же со сроком ожидания `wait`.
pub(super) fn second_generation_opening_with(
    duration: PushedDuration,
    wait: Duration,
) -> (Harness, SessionRef, PushedStreamWriter, PushedStreamWriter) {
    let (harness, current, first_writer) = loaded_harness();
    let mut harness = with_wait(harness, wait);
    let (second_writer, second) = generation_with_head(&current, 2, duration);
    harness.media.on_intake(vec![second], None);
    (harness, current, first_writer, second_writer)
}

/// Главное (решение 146): клик туда, где у нового поколения данных нет, — поколение
/// ждёт данных, прежнее видео играет и не двигается; страницу просят перемотаться
/// ровно один раз, часы страницы ждут в цели, окно видит ожидание (спиннер). Байты у
/// цели пришли — поколение уходит плееру уже на цели.
#[test]
fn a_click_where_the_new_generation_has_no_data_waits_for_the_data_there() {
    let (mut harness, _current, _first, mut second) =
        second_generation_opening(PushedDuration::Finite(LONG_CLIP));
    let target = Duration::from_secs(300);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(target), Instant::now());

    let calls = harness.drive_for(HOLD_CHECK);
    assert_eq!(
        harness.load_count(),
        1,
        "handed over without data: {calls:?}"
    );
    assert!(
        !calls.iter().any(|call| matches!(call, PlayerCall::Seek(_))),
        "the old video was moved: {calls:?}"
    );
    assert_eq!(
        harness.media.generation_wait(),
        GenerationWait::AwaitingDataAtTarget
    );
    let sent = harness.page.take_sent();
    assert_eq!(page_seek_targets(&sent), vec![target]);
    let waiting_clocks: Vec<(u64, ClockState)> = clocks(&sent);
    assert!(
        !waiting_clocks.is_empty()
            && waiting_clocks
                .iter()
                .all(|clock| *clock == (300_000_000, ClockState::Waiting)),
        "the page clock must wait at the target: {waiting_clocks:?}"
    );

    append_media_at(&mut second, target);
    harness.drive_until_loaded();
    assert_eq!(start_of_load(&harness, 1), Some(target));
    assert_eq!(harness.media.generation_wait(), GenerationWait::Idle);
    harness.show_last_load(target);
    harness.drive_for(Duration::from_millis(100));
    assert!(
        page_seek_targets(&harness.page.take_sent()).is_empty(),
        "the install must not ask the page a second time"
    );
}

/// Страница, пока поколение ждёт, перемоталась сама в другое место (YouTube вернулся на
/// своё): поколение уходит плееру там, где данные страницы, а когда оно показано —
/// клик зрителя повторяется странице и плееру один раз (решение 149: клик главнее
/// самовольного возврата). Итого на клик ровно два `seek` странице.
#[test]
fn a_page_seek_elsewhere_while_the_generation_waits_loads_it_there_then_repeats_the_click() {
    let (mut harness, current, _first, mut second) =
        second_generation_opening(PushedDuration::Finite(LONG_CLIP));
    let click = Duration::from_secs(300);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(click), Instant::now());
    harness.drive_for(Duration::from_millis(100));

    let page_target = Duration::from_secs(120);
    // Как переходник: источник узнаёт о перемотке страницы в порядке провода.
    second.note_page_seek(page_target);
    harness.media.on_intake(
        vec![intent(
            &current,
            PageIntent::FollowPageSeek(page_time(page_target)),
        )],
        None,
    );
    append_media_at(&mut second, page_target);
    harness.drive_until_loaded();

    assert_eq!(start_of_load(&harness, 1), Some(page_target));
    harness.show_last_load(page_target);
    let mut after_show = harness.player.take_calls();
    after_show.extend(harness.drive_for(Duration::from_millis(100)));
    assert_eq!(
        player_seeks(&after_show),
        vec![click],
        "the click is repeated to the player once the generation is shown"
    );
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        vec![click, click],
        "the click, then exactly one repeat of it"
    );
}

/// Страница исполнила нашу просьбу и сказала об этом своей перемоткой рядом с целью
/// (в допуске): это эхо, а не новая цель — поколение встаёт на цель зрителя.
#[test]
fn a_page_seek_matching_the_viewer_request_does_not_retarget() {
    let (mut harness, current, _first, mut second) =
        second_generation_opening(PushedDuration::Finite(LONG_CLIP));
    let target = Duration::from_secs(300);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(target), Instant::now());
    let echo = target + Duration::from_millis(120);
    second.note_page_seek(echo);
    harness.media.on_intake(
        vec![intent(
            &current,
            PageIntent::FollowPageSeek(page_time(echo)),
        )],
        None,
    );
    append_media_at(&mut second, target);
    harness.drive_until_loaded();

    assert_eq!(start_of_load(&harness, 1), Some(target));
    assert_eq!(page_seek_targets(&harness.page.take_sent()), vec![target]);
}

/// Данные у цели так и не пришли: после срока (тот же, что у перемотки плеера; здесь —
/// укороченный) поколение уходит плееру туда, где данные есть, а не ждёт вечно.
#[test]
fn without_data_at_the_target_the_generation_starts_where_data_exists_after_the_wait() {
    let wait = Duration::from_millis(400);
    let (mut harness, _current, _first, _second) =
        second_generation_opening_with(PushedDuration::Finite(LONG_CLIP), wait);
    let clicked_at = Instant::now();
    harness.media.on_viewer_transport(
        ViewerTransportIntent::Seek(Duration::from_secs(300)),
        clicked_at,
    );
    harness.drive_until_loaded();

    let waited = clicked_at.elapsed();
    assert!(waited >= wait, "handed over before the wait ended");
    // Срок взят из политики медиа-части, а не из умолчания (10 с).
    assert!(
        waited < Duration::from_secs(5),
        "the configured wait was ignored"
    );
    // Данные лежат с нуля — «где данные есть» — это начало ролика.
    assert_eq!(start_of_load(&harness, 1), None);
}

/// Страница ушла, пока поколение ждало данных у цели: байтов больше не будет — ждать
/// срок незачем, поколение сразу встаёт туда, где данные есть (доигрывание).
#[test]
fn when_the_page_leaves_the_waiting_generation_starts_where_data_exists_at_once() {
    let (mut harness, current, _first, _second) =
        second_generation_opening(PushedDuration::Finite(LONG_CLIP));
    harness.media.on_viewer_transport(
        ViewerTransportIntent::Seek(Duration::from_secs(300)),
        Instant::now(),
    );
    harness.drive_for(HOLD_CHECK);
    assert_eq!(harness.load_count(), 1);

    harness.media.on_session_ended(&current, TAB_GONE_END);
    let ended_at = Instant::now();
    harness.drive_until_loaded();
    assert!(
        ended_at.elapsed() < Duration::from_millis(PlayerSeekConfig::default().commit_timeout_ms),
        "the generation waited for a page that is gone"
    );
    assert_eq!(start_of_load(&harness, 1), None);
}

/// Требование 147: хаотичная серия кликов, пока поколение открывается и ждёт, — каждое
/// действие зрителя даёт ровно одну просьбу странице, прежнее видео не двигается, а
/// поколение встаёт на ПОСЛЕДНИЙ клик, как только там есть данные. Пауза и скорость
/// посреди серии ожидание не ломают.
#[test]
fn a_chaotic_click_series_ends_at_the_last_click_with_one_page_seek_per_click() {
    let (mut harness, _current, _first, mut second) =
        second_generation_opening(PushedDuration::Finite(LONG_CLIP));
    let clicks = [60, 420, 180, 540, 240].map(Duration::from_secs);
    let mut player_calls = Vec::new();
    for (index, click) in clicks.iter().enumerate() {
        harness
            .media
            .on_viewer_transport(ViewerTransportIntent::Seek(*click), Instant::now());
        if index == 2 {
            harness
                .media
                .on_viewer_transport(ViewerTransportIntent::Pause, Instant::now());
            harness.media.on_viewer_transport(
                ViewerTransportIntent::SetRate(PlayerPlaybackRate::new(1.5).expect("rate")),
                Instant::now(),
            );
            harness
                .media
                .on_viewer_transport(ViewerTransportIntent::Play, Instant::now());
        }
        player_calls.extend(harness.drive_for(Duration::from_millis(60)));
    }
    assert_eq!(harness.load_count(), 1, "handed over without data");
    assert!(
        !player_calls
            .iter()
            .any(|call| matches!(call, PlayerCall::Seek(_))),
        "the old video was moved: {player_calls:?}"
    );

    let last_click = clicks[clicks.len() - 1];
    append_media_at(&mut second, last_click);
    harness.drive_until_loaded();
    assert_eq!(start_of_load(&harness, 1), Some(last_click));
    harness.show_last_load(last_click);
    harness.drive_for(Duration::from_millis(100));
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        clicks.to_vec(),
        "exactly one page seek per click, in click order"
    );
}

/// Требование 147: смена качества (ещё один новый `MediaSource`) посреди ожидания.
/// Поколение 3 вытесняет ждущее поколение 2; страница называет ему позицию сама — и
/// оно встаёт там, как только там есть данные. Странице — одна просьба на один клик.
#[test]
fn a_quality_change_while_a_click_waits_lands_the_newest_generation_at_the_page_position() {
    let (mut harness, current, _first, _second) =
        second_generation_opening(PushedDuration::Finite(LONG_CLIP));
    let target = Duration::from_secs(300);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(target), Instant::now());
    harness.drive_for(Duration::from_millis(100));

    let (mut third, third_notice) =
        generation_with_head(&current, 3, PushedDuration::Finite(LONG_CLIP));
    third.note_page_seek(target);
    harness.media.on_intake(
        vec![
            third_notice,
            intent(&current, PageIntent::FollowPageSeek(page_time(target))),
        ],
        None,
    );
    let calls = harness.drive_for(HOLD_CHECK);
    assert_eq!(
        harness.load_count(),
        1,
        "handed over without data: {calls:?}"
    );
    append_media_at(&mut third, target);
    harness.drive_until_loaded();

    assert_eq!(start_of_load(&harness, 1), Some(target));
    assert_eq!(page_seek_targets(&harness.page.take_sent()), vec![target]);
}

/// Требование 147: многочасовой ролик — цель через три часа ведёт себя так же.
#[test]
fn a_click_hours_into_a_long_video_waits_for_the_data_there() {
    let (mut harness, _current, _first, mut second) =
        second_generation_opening(PushedDuration::Finite(Duration::from_secs(5 * 3_600)));
    let target = Duration::from_secs(3 * 3_600 + 17);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(target), Instant::now());
    harness.drive_for(HOLD_CHECK);
    assert_eq!(harness.load_count(), 1, "handed over without data");

    append_media_at(&mut second, target);
    harness.drive_until_loaded();
    assert_eq!(start_of_load(&harness, 1), Some(target));
}

/// Эфир (длительность `Live`) не позиционируется (правило 03e): ворот нет, поколение
/// уходит плееру сразу и начинается там, где лежат байты страницы (живой край).
/// Странице — одна просьба на клик.
#[test]
fn a_live_generation_is_not_held_and_starts_where_the_page_bytes_are() {
    let (mut harness, _current, _first, _second) = second_generation_opening(PushedDuration::Live);
    let target = Duration::from_secs(300);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(target), Instant::now());
    harness.drive_until_loaded();

    assert_eq!(start_of_load(&harness, 1), None);
    assert_eq!(page_seek_targets(&harness.page.take_sent()), vec![target]);
}

/// Длительность неизвестна (страница ещё не сказала): поток неперематываемый, ворот
/// нет — поколение уходит плееру сразу с начала, а не ждёт.
#[test]
fn a_generation_of_unknown_duration_is_not_held() {
    let (mut harness, _current, _first, _second) =
        second_generation_opening(PushedDuration::Unknown);
    harness.media.on_viewer_transport(
        ViewerTransportIntent::Seek(Duration::from_secs(300)),
        Instant::now(),
    );
    harness.drive_until_loaded();
    assert_eq!(start_of_load(&harness, 1), None);
}

/// Клик туда, где у нового поколения данные уже есть: страницу не просят вовсе (то же
/// правило, что у перемотки плеера), поколение встаёт на клик сразу.
#[test]
fn a_click_on_data_the_new_generation_already_has_asks_the_page_nothing() {
    let (mut harness, _current, _first, mut second) =
        second_generation_opening(PushedDuration::Finite(LONG_CLIP));
    let target = Duration::from_secs(300);
    append_media_at(&mut second, target);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(target), Instant::now());
    harness.drive_until_loaded();

    assert_eq!(start_of_load(&harness, 1), Some(target));
    assert!(page_seek_targets(&harness.page.take_sent()).is_empty());
}
