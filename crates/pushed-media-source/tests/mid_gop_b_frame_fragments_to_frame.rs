//! Фрагменты, разрезанные посреди группы кадров, у видео с B-кадрами — «до кадра»
//! (сессия 16b2 моста, живая находка TikTok в Firefox).
//!
//! TikTok кладёт ролик в SourceBuffer несколькими `appendBuffer`, по одному `moof`+`mdat`
//! на вызов, и режет их там, где кончились скачанные байты, а не по ключевым кадрам:
//! ключевой кадр — только в начале первого фрагмента. Видео — H.264 с B-кадрами: кадр P
//! декодируется раньше своих B, а показывается после них. Если стык попал внутрь такой
//! группы (P остался в предыдущем фрагменте, его B — в следующем), отрезки ВРЕМЕНИ ПОКАЗА
//! соседних фрагментов накладываются на глубину перестановки (80–160 мс), хотя по времени
//! декодирования они идут строго встык.
//!
//! Полка принимала такое наложение за повтор и выбрасывала предыдущий фрагмент. При
//! передаче вкладки весь журнал страницы приходит одной пачкой ещё до первой перемотки
//! плеера, а перемотка (плеер встаёт на позицию страницы) начинает подачу с полки заново —
//! уже без выброшенного:
//! - первый стык режет группу — пропадал фрагмент с единственным ключевым кадром, и видео
//!   вступало с далёкого фрагмента (первый кадр на 10,5 с при позиции 0);
//! - первый стык чистый, а следующий режет группу — первые фрагменты играли, потом картинка
//!   вставала у дыры на месте выброшенного, а звук доигрывал до конца.
//!
//! Фикстуры `video-h264-64-bframes-midgop-{13,14}.mp4` (см. `fixtures/generate.sh`): одни
//! и те же 60 кадров по 40 мс с одним ключевым, разрезанные по 13 и по 14 кадров. Звук —
//! первые шесть фрагментов `audio-aac-4s.mp4` (2,43 с). Настоящие декодеры, настоящее время.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::time::Duration;

use media_core::{DemuxSeekRequest, Demuxer, MediaTime};
use player_core::{
    PlaybackState, PreparedDemuxSeekLandingPolicy, PreparedInitialPosition, PreparedMedia,
};
use pushed_media_source::{
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamMonitor,
    PushedStreamOpener, PushedStreamWriter, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::packet_order::{PacketLoggingDemuxer, ReadVideoPacket, VideoPacketLog};
use support::playback::{PlaybackReport, PresentedFrame, open_stream, play_controlled};

/// Длительность кадра фикстур (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Сколько кадров в ролике.
const FRAMES_IN_CLIP: usize = 60;
/// Время показа первого кадра: B-кадры сдвигают показ на два кадра от декодирования.
/// Оно же — позиция страницы при передаче (TikTok стоит на первом кадре видео).
const FIRST_FRAME: Duration = Duration::from_millis(80);
/// Сколько кадров за прогон плеер вправе выбросить как опоздавшие (поток теста стоял под
/// нагрузкой). На свободной машине показаны все 60; пропавший фрагмент — это 13–14 кадров.
const LATE_FRAME_ALLOWANCE: usize = 3;
/// Сколько фрагментов звука длиной ~0,405 с покрывают ролик.
const AUDIO_FRAGMENTS: usize = 6;
const VIDEO: LaneId = LaneId::new(0);
const AUDIO: LaneId = LaneId::new(1);

/// Время показа кадра номер `index` (по порядку показа).
fn frame_time(index: usize) -> Duration {
    FIRST_FRAME + FRAME * u32::try_from(index).expect("small frame index")
}

/// Где страница разрезала видео на фрагменты.
#[derive(Debug, Clone, Copy)]
enum FragmentCut {
    /// По 13 кадров: первый стык — между группами кадров (отрезки показа соседей не
    /// накладываются), остальные три режут группу. Так выглядел «стоп у стыка».
    FirstBoundaryBetweenGroups,
    /// По 14 кадров: каждый стык режет группу, начиная с первого. Так пропадал
    /// фрагмент с ключевым кадром.
    EveryBoundaryInsideAGroup,
}

impl FragmentCut {
    /// Оба разреза — тесты «до кадра» идут по каждому.
    const ALL: [Self; 2] = [
        Self::FirstBoundaryBetweenGroups,
        Self::EveryBoundaryInsideAGroup,
    ];

    /// Файл фикстуры с таким разрезом.
    const fn fixture(self) -> &'static str {
        match self {
            Self::FirstBoundaryBetweenGroups => "video-h264-64-bframes-midgop-13.mp4",
            Self::EveryBoundaryInsideAGroup => "video-h264-64-bframes-midgop-14.mp4",
        }
    }

    /// Сколько фрагментов видео в фикстуре.
    const fn fragment_count(self) -> usize {
        5
    }
}

/// Страница после передачи вкладки: весь её журнал уже лежит в потоке.
struct HandedOffPage {
    writer: PushedStreamWriter,
    monitor: PushedStreamMonitor,
}

impl HandedOffPage {
    /// Повтор журнала страницы одной пачкой: init обеих полос, каждый фрагмент видео —
    /// отдельным `appendBuffer`, звук и `endOfStream`. Плеер ещё ничего не читал.
    fn replay_journal(cut: FragmentCut) -> (Self, PushedStreamOpener) {
        let policy = PushedSourcePolicy::recommended().expect("policy");
        let (mut writer, opener) = pushed_stream(policy).expect("stream");
        writer
            .add_lane(VIDEO, PushedContainer::FragmentedMp4)
            .expect("video lane");
        writer
            .add_lane(AUDIO, PushedContainer::FragmentedMp4)
            .expect("audio lane");
        writer.set_duration(PushedDuration::Finite(frame_time(FRAMES_IN_CLIP)));

        let video = FragmentedMp4::split(&own_fixture(cut.fixture()));
        let audio = FragmentedMp4::split(&own_fixture("audio-aac-4s.mp4"));
        assert_eq!(
            video.fragments.len(),
            cut.fragment_count(),
            "{cut:?}: the fixture is cut into separate fragments"
        );
        writer
            .append(VIDEO, &video.initialization)
            .expect("video init");
        writer
            .append(AUDIO, &audio.initialization)
            .expect("audio init");
        for fragment in &video.fragments {
            writer.append(VIDEO, fragment).expect("video fragment");
        }
        for fragment in &audio.fragments[..AUDIO_FRAGMENTS] {
            writer.append(AUDIO, fragment).expect("audio fragment");
        }
        writer.end_of_stream().expect("end of stream");
        let page = Self {
            monitor: writer.monitor(),
            writer,
        };
        (page, opener)
    }
}

/// Открывает поток и ставит его на позицию страницы, как процесс браузерного режима
/// (`app-egui` `generation_start::position_segments_demuxer`): первая перемотка плеера
/// происходит уже ПОСЛЕ того, как весь журнал лёг на полку. Прочитанные пакеты видео
/// пишутся в `log`.
fn open_at_page_position(
    page: &HandedOffPage,
    opener: &mut PushedStreamOpener,
    log: &VideoPacketLog,
) -> PreparedMedia {
    page.writer.note_page_seek(FIRST_FRAME);
    let mut demuxer = open_stream(opener).demuxer;
    let result = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(FIRST_FRAME))
        .expect("the stream seeks without waiting");
    let target_position = MediaTime::from_duration(FIRST_FRAME);
    assert!(
        result.actual_position <= target_position,
        "landing {:?} after the page position {FIRST_FRAME:?}",
        result.actual_position
    );
    let logged = PacketLoggingDemuxer::new(demuxer, log.clone());
    PreparedMedia::from_external_label("pushed media mid-GOP fragments", Box::new(logged))
        .with_prepared_initial_position(PreparedInitialPosition::PositionedAt {
            target_position,
            result,
            landing_policy: PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        })
        .expect("a landing before the target is a valid start position")
}

/// Плеер прочитал каждый кадр ролика ровно один раз и строго в порядке декодирования:
/// ни один фрагмент не пропал и не пришёл дважды.
fn assert_every_frame_read_once_in_decode_order(cut: FragmentCut, packets: &[ReadVideoPacket]) {
    let mut presentation_times: Vec<Duration> = packets
        .iter()
        .map(|packet| packet.presentation_time)
        .collect();
    presentation_times.sort_unstable();
    let expected: Vec<Duration> = (0..FRAMES_IN_CLIP).map(frame_time).collect();
    assert_eq!(
        presentation_times, expected,
        "{cut:?}: every frame of every fragment reaches the decoder exactly once"
    );
    let out_of_order: Vec<_> = packets
        .windows(2)
        .filter(|pair| pair[1].decode_time <= pair[0].decode_time)
        .collect();
    assert!(
        out_of_order.is_empty(),
        "{cut:?}: packets left decode order: {out_of_order:?}"
    );
    // Порядок декодирования действительно расходится с показом: иначе фикстура не
    // проверяла бы ничего.
    assert!(
        packets
            .windows(2)
            .any(|pair| pair[1].presentation_time < pair[0].presentation_time),
        "{cut:?}: the fixture must reorder frames (B-frames)"
    );
}

/// Показаны кадры от `first` до последнего кадра ролика: по порядку, без повторов, без
/// дыр длиннее одного кадра и почти все до единого ([`LATE_FRAME_ALLOWANCE`]).
fn assert_frames_shown_through_the_end(cut: FragmentCut, shown: &[PresentedFrame], first: usize) {
    let times: Vec<Duration> = shown.iter().map(|frame| frame.pts).collect();
    assert_eq!(
        times.first().copied(),
        Some(frame_time(first)),
        "{cut:?}: the first frame shown: {times:?}"
    );
    assert_eq!(
        times.last().copied(),
        Some(frame_time(FRAMES_IN_CLIP - 1)),
        "{cut:?}: playback reaches the last frame: {times:?}"
    );
    assert!(
        times.windows(2).all(|pair| pair[0] < pair[1]),
        "{cut:?}: frames are not in order: {times:?}"
    );
    let widest_step = times
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .max()
        .unwrap_or_default();
    assert!(
        widest_step <= FRAME * 2,
        "{cut:?}: frames skip {widest_step:?} of time: {times:?}"
    );
    assert!(
        times.len() + LATE_FRAME_ALLOWANCE >= FRAMES_IN_CLIP - first,
        "{cut:?}: only {} of {} frames were shown: {times:?}",
        times.len(),
        FRAMES_IN_CLIP - first
    );
}

/// Плеер доиграл без ошибок.
fn assert_played_to_the_end(cut: FragmentCut, report: &PlaybackReport) {
    assert!(
        report.errors.is_empty(),
        "{cut:?}: player errors: {:?}",
        report.errors
    );
    assert_eq!(
        report.final_state,
        PlaybackState::Ended,
        "{cut:?}: the picture stopped at {:?}",
        report.frames.last().map(|frame| frame.pts)
    );
}

/// Кадры, впервые показанные после команды перемотки `seek_index`.
fn frames_after_seek(report: &PlaybackReport, seek_index: usize) -> Vec<PresentedFrame> {
    let (seek_tick, _) = report.seeks[seek_index];
    report
        .frames
        .iter()
        .filter(|frame| frame.tick > seek_tick)
        .copied()
        .collect()
}

/// Передача вкладки: журнал пришёл пачкой, плеер встал на позицию страницы и играет.
/// Каждый кадр каждого фрагмента обязан дойти до декодера и до показа — и через чистый
/// стык, и через стык, разрезавший группу кадров.
#[test]
fn a_reel_cut_inside_groups_of_pictures_plays_every_frame_after_the_handoff() {
    for cut in FragmentCut::ALL {
        let (page, mut opener) = HandedOffPage::replay_journal(cut);
        let log = VideoPacketLog::default();
        let prepared = open_at_page_position(&page, &mut opener, &log);

        let report = play_controlled(prepared, |_control| {});

        assert_played_to_the_end(cut, &report);
        assert_every_frame_read_once_in_decode_order(cut, &log.packets());
        assert_frames_shown_through_the_end(cut, &report.frames, 0);
        // Звук доиграл весь ролик вместе с картинкой (а не один, пока она стоит).
        let expected_audio = frame_time(FRAMES_IN_CLIP) - FIRST_FRAME;
        assert!(
            report.audio_duration().abs_diff(expected_audio) <= Duration::from_millis(150),
            "{cut:?}: audio {:?}, expected about {expected_audio:?}",
            report.audio_duration()
        );
        assert_eq!(
            page.monitor.take_page_seek_request(),
            None,
            "{cut:?}: everything is on the shelf, the page is not asked to seek"
        );
    }
}

/// Ролик доигран, страница перематывается в начало (TikTok крутит ролик по кругу):
/// полка обязана всё ещё держать КАЖДЫЙ фрагмент — второй круг показывает все кадры
/// заново, и страницу ни о чём не просят (она то, что у неё лежит, не пересылает).
#[test]
fn a_page_seek_to_the_start_after_the_end_replays_every_frame() {
    for cut in FragmentCut::ALL {
        let (page, mut opener) = HandedOffPage::replay_journal(cut);
        let log = VideoPacketLog::default();
        let prepared = open_at_page_position(&page, &mut opener, &log);

        let mut packets_before_replay = None;
        let report = play_controlled(prepared, |control| {
            if packets_before_replay.is_none() && control.state() == PlaybackState::Ended {
                packets_before_replay = Some(log.packets().len());
                // `seek_started{page_seek}`: приложение отмечает цель страницы и командует
                // плееру туда же; после конца плеер стоит на паузе — «играть» как кнопка.
                page.writer.note_page_seek(Duration::ZERO);
                control.seek(Duration::ZERO);
                control.play();
            }
        });

        let packets_before_replay =
            packets_before_replay.unwrap_or_else(|| panic!("{cut:?}: the first round never ended"));
        assert_played_to_the_end(cut, &report);
        let packets = log.packets();
        assert_every_frame_read_once_in_decode_order(cut, &packets[..packets_before_replay]);
        assert_every_frame_read_once_in_decode_order(cut, &packets[packets_before_replay..]);
        assert_frames_shown_through_the_end(cut, &frames_after_seek(&report, 0), 0);
        assert_eq!(
            page.monitor.take_page_seek_request(),
            None,
            "{cut:?}: the replay comes from the shelf"
        );
    }
}

/// Перемотка плеера в время, которое по показу принадлежит сразу двум фрагментам (стык
/// разрезал группу кадров): кадр цели декодируется только от ключевого кадра в первом
/// фрагменте, и все фрагменты между ними обязаны лежать на полке. Кадры идут от цели до
/// конца ролика.
#[test]
fn a_seek_into_the_time_shared_by_two_fragments_shows_the_target_frame() {
    let cut = FragmentCut::FirstBoundaryBetweenGroups;
    // Стык фрагментов 2 и 3: показ третьего начинается с кадра 25, а показ второго кончается
    // кадром 28 (это кадр P, оставшийся во втором). Кадр 27 лежит внутри общего времени.
    let target_frame = 27;
    let target = frame_time(target_frame);
    let (page, mut opener) = HandedOffPage::replay_journal(cut);
    let log = VideoPacketLog::default();
    let prepared = open_at_page_position(&page, &mut opener, &log);

    let mut seeked = false;
    let report = play_controlled(prepared, |control| {
        if !seeked && control.observation().position >= frame_time(5) {
            seeked = true;
            control.seek(target);
        }
    });

    assert!(seeked, "the clip played long enough to seek");
    assert_played_to_the_end(cut, &report);
    assert_frames_shown_through_the_end(cut, &frames_after_seek(&report, 0), target_frame);
    assert_eq!(
        page.monitor.take_page_seek_request(),
        None,
        "the target is on the shelf"
    );
}
