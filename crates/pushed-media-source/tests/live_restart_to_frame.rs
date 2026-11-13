//! Эфир, перезапущенный страницей в том же `MediaSource`, «до кадра» (сессия 09b2 моста).
//!
//! Живая находка (Twitch, Chromium, запись потока страницы): на паузе страница стирает весь
//! буфер и ставит элемент на 0, на play кладёт эфир заново с ~0 и перематывается к началу
//! своих байтов. Звук шёл, а картинка стояла ~24 с. Корень — полка не повторяла стирание
//! MSE и держала старую шкалу вперемешку с новой:
//! - MSE стирает до следующего ключевого кадра, а страница называет только свой диапазон.
//!   Подчистка прошлого оставляла на полке хвосты групп кадров без ключевых кадров;
//! - стёртое, но ещё не сыгранное лежало и после того, как плеер ушёл перемоткой.
//!
//! Перемотка возвращала всё это в очередь, подача уходила в старые хвосты (их кадры
//! декодировать не с чего), курсор вставал в конце старой шкалы, а новые байты оказывались
//! «в прошлом» и не подавались.
//!
//! Второй корень — шов между эпизодами: Twitch стартует в одном качестве и переходит на
//! другое. У старого качества кадры показываются с большей задержкой, чем декодируются,
//! поэтому ключевой кадр нового качества показывается раньше последнего выданного кадра,
//! хотя декодируется строго после него. Шов считал его повтором и выбрасывал группу кадров.
//!
//! Фикстуры: `video-h264-64-bframes-per-frame.mp4` — 40 кадров по одному на фрагмент, группа
//! из 8 кадров, показ с 0,08 с (ключевые кадры показа: 0,08; 0,40; 0,72; 1,04; 1,36 с);
//! `video-h264-64-4s.mp4` — без B-кадров, ключевой кадр в начале каждого фрагмента на 0,4 с.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::time::Duration;

use player_core::{PlaybackState, PreparedMedia};
use pushed_media_source::{
    LaneId, PageRemovedRange, PageRemovedRangeEnd, PushedContainer, PushedDuration,
    PushedSourcePolicy, PushedStreamDemuxer, PushedStreamOpener, PushedStreamWriter, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::playback::{PlaybackReport, PresentedFrame, open, open_stream, play_controlled};

/// Ролик с B-кадрами по одному кадру на фрагмент: 40 кадров в порядке декодирования.
const LOW_LATENCY_CLIP: &str = "video-h264-64-bframes-per-frame.mp4";
/// Ролик без B-кадров: 10 фрагментов по 0,4 с.
const WHOLE_FRAGMENTS_CLIP: &str = "video-h264-64-4s.mp4";
/// Длительность кадра обоих роликов.
const FRAME: Duration = Duration::from_millis(40);
/// Длительность ролика без B-кадров.
const CLIP: Duration = Duration::from_secs(4);
/// Конец ролика без B-кадров: начало его последнего кадра.
const LAST_FRAME: Duration = Duration::from_millis(3_960);
/// Полоса видео.
const VIDEO: LaneId = LaneId::new(0);
/// Подчистки прошлого страницей (мс): как `remove(buffered.start, …)` у Twitch, каждая
/// начинается с ключевого кадра и стирает его, а MSE сносит и хвост до следующего ключевого.
const PAST_TRIMS_MS: [(u64, u64); 4] = [(80, 120), (400, 440), (720, 760), (1_040, 1_080)];
/// Страница стирает весь оставшийся буфер: от следующего ключевого кадра и дальше конца.
const UNLOAD_MS: (u64, u64) = (1_360, 10_000);
/// Докуда сыграла старая шкала: последний фрагмент новой шкалы до хвоста старой кончается
/// на 1,2 с, а хвост начинается у позиции — полка подала бы его как продолжение.
const OLD_TIMELINE_PLAYED_TO: Duration = Duration::from_millis(1_220);
/// Вторая перемотка страницы — внутрь её новых байтов (второй кадр новой шкалы).
const NEW_TIMELINE_SEEK: Duration = FRAME;
/// Сколько шагов плеера страница грузит эфир заново.
const RELOAD_TICKS: usize = 20;
/// Сколько шагов после второй перемотки идут остальные байты новой шкалы.
const REST_OF_RELOAD_TICKS: usize = 40;
/// Сколько кадров ролика с B-кадрами идёт до смены качества: декодируются раньше 1,2 с, а
/// показываются вплоть до 1,32 с.
const FRAMES_BEFORE_SWITCH: usize = 30;
/// С какого фрагмента ролика без B-кадров идёт новое качество (1,2 с).
const SWITCH_FRAGMENT: usize = 3;
/// Первый кадр нового качества после всех выданных кадров старого: вокруг смены кадры
/// дублируются, и плеер вправе не показать новые, опоздавшие на доли кадра.
const FIRST_FRAME_PAST_THE_OLD_QUALITY: Duration = Duration::from_millis(1_360);

/// Страница с одной полосой видео.
struct Page {
    writer: PushedStreamWriter,
}

impl Page {
    /// Поток с указанной длительностью.
    fn new(duration: PushedDuration) -> (Self, PushedStreamOpener) {
        let policy = PushedSourcePolicy::recommended().expect("policy");
        let (mut writer, opener) = pushed_stream(policy).expect("stream");
        writer
            .add_lane(VIDEO, PushedContainer::FragmentedMp4)
            .expect("video lane");
        writer.set_duration(duration);
        (Self { writer }, opener)
    }

    /// `appendBuffer`: init и фрагменты.
    fn append(&mut self, initialization: &[u8], fragments: &[Vec<u8>]) {
        self.writer
            .append(VIDEO, initialization)
            .expect("initialization");
        self.append_fragments(fragments);
    }

    /// `appendBuffer`: только фрагменты.
    fn append_fragments(&mut self, fragments: &[Vec<u8>]) {
        for fragment in fragments {
            self.writer.append(VIDEO, fragment).expect("fragment");
        }
    }

    /// `SourceBuffer.remove(start, end)`.
    fn remove(&mut self, (start_ms, end_ms): (u64, u64)) {
        self.writer
            .note_page_removed(
                VIDEO,
                PageRemovedRange::new(
                    Duration::from_millis(start_ms),
                    PageRemovedRangeEnd::At(Duration::from_millis(end_ms)),
                ),
            )
            .expect("removal");
    }

    /// Подчистки прошлого и стирание всего, что осталось: «выгрузка» эфира.
    fn unload(&mut self) {
        for trim in PAST_TRIMS_MS {
            self.remove(trim);
        }
        self.remove(UNLOAD_MS);
    }
}

fn prepared_live(opener: &mut PushedStreamOpener) -> PreparedMedia {
    let opened = open_stream(opener);
    let port = opened.timeline.expect("live timeline");
    PreparedMedia::from_external_label("live restart test", Box::new(opened.demuxer))
        .with_dynamic_timeline(port)
        .expect("live timeline accepted")
}

fn prepared_plain(demuxer: PushedStreamDemuxer) -> PreparedMedia {
    PreparedMedia::from_external_label("low latency switch test", Box::new(demuxer))
}

/// Кадры, впервые показанные после перемотки номер `seek_index`.
fn frames_after_seek(report: &PlaybackReport, seek_index: usize) -> Vec<PresentedFrame> {
    let (seek_tick, _) = report.seeks[seek_index];
    report
        .frames
        .iter()
        .filter(|frame| frame.tick > seek_tick)
        .copied()
        .collect()
}

/// Картинка идёт без дыр: в `frames` есть кадр каждого момента от `from` до `to`.
fn assert_no_picture_hole(frames: &[PresentedFrame], from: Duration, to: Duration) {
    let mut shown: Vec<Duration> = frames.iter().map(|frame| frame.pts).collect();
    shown.sort_unstable();
    shown.dedup();
    let first = shown.iter().copied().find(|pts| *pts + FRAME > from);
    assert_eq!(first, Some(from), "first frame, shown: {shown:?}");
    assert_eq!(
        shown.last().copied(),
        Some(to),
        "last frame, shown: {shown:?}"
    );
    let hole = shown
        .windows(2)
        .filter(|pair| pair[0] >= from)
        .find(|pair| pair[1] - pair[0] > FRAME);
    assert_eq!(hole, None, "a hole in the picture, shown: {shown:?}");
}

/// Сколько фрагментов новой шкалы приходит до второй перемотки страницы.
#[derive(Clone, Copy)]
struct ReloadedBeforeSeek {
    /// Фрагменты ролика без B-кадров.
    fragments: usize,
}

/// Весь путь Twitch: старая шкала, выгрузка, новая шкала с другим init, перемотка к её
/// началу посреди докачки. Картинка новой шкалы идёт от цели до конца без дыр.
fn restart_plays_the_whole_new_timeline(reloaded: ReloadedBeforeSeek) {
    let old = FragmentedMp4::split(&own_fixture(LOW_LATENCY_CLIP));
    let new = FragmentedMp4::split(&own_fixture(WHOLE_FRAGMENTS_CLIP));
    let (mut page, mut opener) = Page::new(PushedDuration::Live);
    page.append(&old.initialization, &old.fragments);
    let prepared = prepared_live(&mut opener);
    let monitor = page.writer.monitor();

    let mut unloaded_at = None;
    let mut new_seek_at = None;
    let mut rest_sent = false;
    let report = play_controlled(prepared, |control| {
        monitor.note_playback_position(control.observation().position);
        let tick = control.observation().tick;
        match (unloaded_at, new_seek_at) {
            (None, _) if control.observation().position >= OLD_TIMELINE_PLAYED_TO => {
                unloaded_at = Some(tick);
                page.unload();
                // Порядок моста: источник узнаёт о перемотке страницы раньше плеера.
                page.writer.note_page_seek(Duration::ZERO);
                control.seek(Duration::ZERO);
            }
            (Some(at), None) if tick == at + RELOAD_TICKS => {
                page.append(&new.initialization, &new.fragments[..reloaded.fragments]);
                page.writer.note_page_seek(NEW_TIMELINE_SEEK);
                control.seek(NEW_TIMELINE_SEEK);
                new_seek_at = Some(tick);
            }
            (_, Some(at)) if !rest_sent && tick == at + REST_OF_RELOAD_TICKS => {
                rest_sent = true;
                page.append_fragments(&new.fragments[reloaded.fragments..]);
                page.writer.end_of_stream().expect("end of stream");
            }
            _ => {}
        }
    });

    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    assert_eq!(report.final_state, PlaybackState::Ended);
    assert_no_picture_hole(
        &frames_after_seek(&report, 1),
        NEW_TIMELINE_SEEK,
        LAST_FRAME,
    );
}

/// Новые байты кончаются на 0,4 с, а сразу за ними лежали бы хвосты групп кадров, которые
/// страница стёрла вместе с их ключевыми кадрами: подача ушла бы в них до конца старой шкалы.
#[test]
fn a_restart_does_not_continue_into_the_tails_the_page_erased_with_their_keyframes() {
    restart_plays_the_whole_new_timeline(ReloadedBeforeSeek { fragments: 1 });
}

/// Новые байты кончаются на 1,2 с, у самой позиции старой шкалы: сразу за ними лежал бы её
/// стёртый, но не сыгранный хвост.
#[test]
fn a_restart_does_not_continue_into_the_unplayed_tail_the_page_erased() {
    restart_plays_the_whole_new_timeline(ReloadedBeforeSeek { fragments: 3 });
}

/// Смена качества в конце декодирования группы: ключевой кадр нового качества показывается
/// раньше уже выданного кадра старого, но декодируется после него. Это продолжение, а не
/// повтор: новая группа кадров доходит до показа целиком.
#[test]
fn a_quality_switch_that_continues_decoding_keeps_its_group_of_pictures() {
    let low_latency = FragmentedMp4::split(&own_fixture(LOW_LATENCY_CLIP));
    let whole = FragmentedMp4::split(&own_fixture(WHOLE_FRAGMENTS_CLIP));
    let (mut page, mut opener) = Page::new(PushedDuration::Finite(CLIP));
    page.append(
        &low_latency.initialization,
        &low_latency.fragments[..FRAMES_BEFORE_SWITCH],
    );
    page.append(&whole.initialization, &whole.fragments[SWITCH_FRAGMENT..]);
    page.writer.end_of_stream().expect("end of stream");
    let prepared = prepared_plain(open(&mut opener));

    let report = play_controlled(prepared, |_| {});

    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    assert_eq!(report.final_state, PlaybackState::Ended);
    assert_no_picture_hole(&report.frames, FIRST_FRAME_PAST_THE_OLD_QUALITY, LAST_FRAME);
}
