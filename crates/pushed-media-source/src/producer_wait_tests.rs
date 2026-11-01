//! Ответ потока «чего жду» (`producer_wait`): живая страница против всех причин,
//! по которым ждать уже нечего. Настоящие демуксеры эпизодов, фикстура
//! `video-h264-64.mp4` (init + 4 фрагмента по 200 мс).

use std::thread;
use std::time::{Duration, Instant};

use media_core::{DemuxDataWait, DemuxReadEvent, Demuxer};

use crate::buffer_window::BufferWindow;
use crate::container::PushedContainer;
use crate::lane::LaneId;
use crate::policy::PushedSourcePolicy;
use crate::stream::{OpenProgress, PushedStreamDemuxer, PushedStreamOpener, PushedStreamWriter};
use crate::test_fixtures::{SplitMp4, VIDEO_H264_64, split_mp4};
use crate::time::PushedDuration;
use crate::{DownloadCutOutcome, pushed_stream};

const VIDEO: LaneId = LaneId::new(0);
/// Сколько ждать фоновых демуксеров.
const WAIT: Duration = Duration::from_secs(5);

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

/// Поток с одной полосой: init и первый фрагмент уже пришли, демуксер открыт.
fn opened_stream(
    policy: PushedSourcePolicy,
) -> (PushedStreamWriter, PushedStreamDemuxer, SplitMp4) {
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("lane");
    writer.set_duration(PushedDuration::Finite(ms(800)));
    let media = split_mp4(VIDEO_H264_64);
    writer.append(VIDEO, &media.init).expect("init");
    writer
        .append(VIDEO, &media.fragments[0])
        .expect("fragment 0");
    let demuxer = open(&mut opener);
    (writer, demuxer, media)
}

fn open(opener: &mut PushedStreamOpener) -> PushedStreamDemuxer {
    let deadline = Instant::now() + WAIT;
    loop {
        match opener.poll_open().expect("opens") {
            OpenProgress::Ready(opened) => return opened.demuxer,
            OpenProgress::Waiting(hint) => {
                assert!(Instant::now() < deadline, "stream did not open");
                thread::sleep(hint.retry_after());
            }
        }
    }
}

/// Читает, пока поток не скажет «данных нет» (всё прочитано).
fn read_until_dry(demuxer: &mut PushedStreamDemuxer) {
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "the stream never ran dry");
        match demuxer.next_event().expect("readable") {
            DemuxReadEvent::TemporarilyUnavailable(_) => return,
            DemuxReadEvent::EndOfStream => panic!("the stream ended instead of waiting"),
            _ => {}
        }
    }
}

fn policy() -> PushedSourcePolicy {
    PushedSourcePolicy::recommended().expect("policy")
}

/// Перемотка вне полки: поток ждёт байтов цели от живой страницы — честное ожидание.
#[test]
fn an_off_shelf_seek_awaits_the_live_page() {
    let (_writer, mut demuxer, _media) = opened_stream(policy());
    demuxer.seek(ms(600)).expect("seek off the shelf");
    read_until_dry(&mut demuxer);
    assert!(demuxer.data_wait().is_live_producer());
}

/// Страница сказала `endOfStream`: байтов больше не будет — ждать нечего.
#[test]
fn an_honest_end_of_stream_is_not_a_live_wait() {
    let (mut writer, demuxer, _media) = opened_stream(policy());
    assert!(demuxer.data_wait().is_live_producer());
    writer.end_of_stream().expect("end");
    assert_eq!(demuxer.data_wait(), DemuxDataWait::Unattributed);
}

/// Перемотка назад после `endOfStream` туда, где байтов нет, снова открывает вход
/// полосы: страница пришлёт цель — ожидание снова честное.
#[test]
fn a_seek_after_the_end_into_missing_data_awaits_the_page_again() {
    let (mut writer, mut demuxer, media) = opened_stream(policy());
    writer
        .append(VIDEO, &media.fragments[3])
        .expect("fragment 3");
    writer.end_of_stream().expect("end");
    assert_eq!(demuxer.data_wait(), DemuxDataWait::Unattributed);
    demuxer.seek(ms(300)).expect("seek into the hole");
    assert!(demuxer.data_wait().is_live_producer());
}

/// Писатель ушёл (вкладка закрылась) — даже если вход открыт заново, ждать некого.
#[test]
fn a_gone_writer_is_not_a_live_wait() {
    let (mut writer, mut demuxer, media) = opened_stream(policy());
    writer
        .append(VIDEO, &media.fragments[3])
        .expect("fragment 3");
    writer.end_of_stream().expect("end");
    demuxer
        .seek(ms(300))
        .expect("seek into the hole reopens the lane");
    assert!(demuxer.data_wait().is_live_producer());
    drop(writer);
    assert_eq!(demuxer.data_wait(), DemuxDataWait::Unattributed);
}

/// Обрыв скачивания: дальше путь досмотра, а не ожидание.
#[test]
fn a_download_cut_short_is_not_a_live_wait() {
    let (mut writer, demuxer, _media) = opened_stream(policy());
    let outcome = writer.end_download_cut_short().expect("cut");
    assert!(matches!(outcome, DownloadCutOutcome::CutShort(_)));
    assert_eq!(demuxer.data_wait(), DemuxDataWait::Unattributed);
}

/// Мёртвый поток (первая ошибка) — ждать нечего: плеер получит ошибку.
#[test]
fn a_failed_stream_is_not_a_live_wait() {
    let (mut writer, demuxer, _media) = opened_stream(policy());
    // Байты в необъявленную полосу — нарушение договора страницей: поток мёртв.
    assert!(writer.append(LaneId::new(9), &[0_u8; 8]).is_err());
    assert!(writer.failure().is_some());
    assert_eq!(demuxer.data_wait(), DemuxDataWait::Unattributed);
}

/// Мы сами держим страницу («полон»): если плееру при этом нечего читать, это наша
/// блокировка, а не честное ожидание — пусть сработает срок плеера.
#[test]
fn a_full_buffer_holding_the_page_is_not_a_live_wait() {
    let tiny_window = BufferWindow { ahead: ms(100) };
    let (_writer, demuxer, _media) =
        opened_stream(policy().with_buffer_window(tiny_window).expect("window"));
    assert_eq!(demuxer.data_wait(), DemuxDataWait::Unattributed);
}

/// Счётчик принесённого (решение 151): после перемотки вне полки каждый кусок для
/// ждущей полосы — даже недособранный сегмент — увеличивает счётчик на свой размер;
/// пока страница молчит, счётчик стоит.
#[test]
fn bytes_for_the_awaited_target_raise_the_progress_even_before_the_segment_is_whole() {
    let (mut writer, mut demuxer, media) = opened_stream(policy());
    demuxer.seek(ms(600)).expect("seek off the shelf");
    read_until_dry(&mut demuxer);
    let progress = |demuxer: &PushedStreamDemuxer| match demuxer.data_wait() {
        DemuxDataWait::LiveProducer { progress } => progress.delivered_bytes(),
        DemuxDataWait::Unattributed => panic!("the live page is awaited"),
    };
    let before = progress(&demuxer);
    assert_eq!(progress(&demuxer), before, "a silent page brings nothing");

    let target_fragment = &media.fragments[3];
    let half = target_fragment.len() / 2;
    writer
        .append(VIDEO, &target_fragment[..half])
        .expect("first half of the target segment");
    assert_eq!(progress(&demuxer), before + half as u64);
    writer
        .append(VIDEO, &target_fragment[half..])
        .expect("second half");
    assert_eq!(progress(&demuxer), before + target_fragment.len() as u64);
}

/// Находка ревью: полка почти полна байтами СТАРОГО места (≥ порога «полон»), и
/// перемотка уходит туда, где данных нет. Честная страница при «полон» молчит; если бы
/// «полон» держался, поток перестал бы отвечать «ждём живую страницу», и перемотка
/// сдалась бы по сроку. После перемотки старое (до цели — «сыгранное») выселяется,
/// поток снова принимает байты и честно ждёт страницу — без единого нового байта.
#[test]
fn an_off_shelf_seek_from_a_shelf_full_of_the_old_place_awaits_the_page_again() {
    let media = split_mp4(crate::test_fixtures::VIDEO_H264_64_4S);
    // Старое место: 0–2,8 с (7 фрагментов), цель 3,4 с — вне полки.
    let old_place = 7;
    let held: usize = media.init.len()
        + media.fragments[..old_place]
            .iter()
            .map(Vec::len)
            .sum::<usize>();
    // Потолок такой, что старое место держит 80 % — выше порога «полон» (75 %).
    let ceiling = held * 100 / 80;
    // Очередь эпизода на один пакет: поток эпизода встаёт сразу, и непрочитанное
    // старое место остаётся в его трубе — такие сегменты предохранитель не трогает.
    let policy = policy()
        .with_max_held_stream_bytes(std::num::NonZeroUsize::new(ceiling).expect("non-zero"))
        .with_episode_demux_limits(demux_api::ProgressiveDemuxBufferLimits::new(
            std::num::NonZeroUsize::MIN,
            std::num::NonZeroUsize::new(64 * 1024).expect("non-zero"),
        ));
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("lane");
    writer.set_duration(PushedDuration::Finite(ms(4_000)));
    writer.append(VIDEO, &media.init).expect("init");
    for fragment in &media.fragments[..old_place] {
        writer.append(VIDEO, fragment).expect("old place");
    }
    let monitor = writer.monitor();
    // Плеер открыл поток, но старое место ещё в трубе эпизода (не дочитано): такие
    // сегменты предохранитель не трогает — памяти это не освободит.
    let mut demuxer = open(&mut opener);
    eprintln!(
        "DEBUG before seek {:?} held {} ceiling {}",
        monitor.buffer_status(),
        monitor.held_segment_bytes(),
        ceiling
    );
    assert_eq!(
        monitor.buffer_status().intake,
        crate::buffer_window::BufferIntake::Full,
        "the old place fills the shelf"
    );

    demuxer.seek(ms(3_400)).expect("seek off the shelf");
    eprintln!(
        "DEBUG after seek {:?} held {}",
        monitor.buffer_status(),
        monitor.held_segment_bytes()
    );
    read_until_dry(&mut demuxer);

    assert_eq!(
        monitor.buffer_status().intake,
        crate::buffer_window::BufferIntake::Accepting,
        "the old place before the target is evicted: the honest page may send again"
    );
    assert!(
        demuxer.data_wait().is_live_producer(),
        "the stream awaits the live page instead of letting the seek time out"
    );
}
