//! Сквозная цепочка «действие зрителя → плеер и страница» на НАСТОЯЩЕМ плеере (04a).
//!
//! Медиа-часть сеанса — настоящая, плеер — настоящий `BrowserPlayback` (рабочий поток
//! `PlayerWorker`, программный FFmpeg, тихая карта звука реального времени, безоконный
//! вывод), поток — настоящий `pushed-media-source` на фикстурах протокола. Подделана
//! только страница: видно каждое сообщение, которое ушло бы ей по мосту.
//!
//! Почему не в настоящем окне: синтетический ввод в `kwin_wayland --virtual` из теста
//! недоступен, а тестовая дверь «нажми паузу» в производственном бинаре — лишний шов.
//! Окно до этой цепочки добавляет только ящик намерений `AppState` и пульт (их цепочка
//! проверена отдельно: `state::window_mode::tests`, `window::driver` tests).
//!
//! Проверяется то, что увидят страница и зритель: пауза зрителя — у страницы `pause` и
//! часы в паузе; запоздалый `play` страницы паузу не снимает; перемотка зрителя на
//! полке — часы на цели и ни одной перемотки странице; вне полки — ровно одна просьба
//! `seek` (её выдаёт источник, а не второй раз медиа-часть).

use std::sync::Arc;

use browser_bridge_protocol::session::player::ClockState;

use super::*;
use crate::browser_session::inbox::BrowserSessionWake;
use crate::browser_session::playback::metered_audio::BrowserAudioOutputChoice;
use crate::browser_session::playback::video_output::HeadlessVideoOutput;
use crate::browser_session::playback::{BrowserPlayback, decoder_thread_config_from_settings};
use crate::browser_session::session_config::BrowserSessionSettings;
use crate::viewer_transport::ViewerTransportIntent;

/// Объявленная длина потока: данных только 400 мс, остальное «у страницы».
const DECLARED_DURATION: Duration = Duration::from_secs(10);
/// Шаг главного цикла теста.
const STEP: Duration = Duration::from_millis(5);
/// Потолок ожидания одного шага сценария.
const SCENARIO_DEADLINE: Duration = Duration::from_secs(20);

/// Будильник без цикла: тест крутит медиа-часть сам.
struct NoWake;

impl BrowserSessionWake for NoWake {
    fn wake(&self) {}
}

/// Медиа-часть на настоящем плеере.
struct RealChain {
    media: SessionMedia,
    page: FakePage,
    /// Всё, что получила страница, по порядку.
    seen: Vec<PlayerSessionMessage>,
}

impl RealChain {
    fn start() -> Self {
        let mut config = fastiplayer_config::AppConfig::default();
        // Как у тестов плеера: фиксированные потоки FFmpeg, одинаково на любой машине.
        config.video.sw_decode_threads = 2;
        let settings = BrowserSessionSettings::from_app_config_for_test(config);
        let playback = BrowserPlayback::start(
            &settings,
            Box::new(HeadlessVideoOutput::new(
                decoder_thread_config_from_settings(&settings),
            )),
            BrowserAudioOutputChoice::SimulatedRealTime,
            Arc::new(NoWake),
            Instant::now(),
        )
        .expect("browser playback starts");
        let page = FakePage::default();
        let media = SessionMedia::new(
            Box::new(playback),
            Box::new(FakeCloser::default()),
            PageOutbound::new(Box::new(page.clone())),
        );
        Self {
            media,
            page,
            seen: Vec::new(),
        }
    }

    /// Крутит медиа-часть, пока страница не увидит нужное (или до потолка).
    fn drive_until(&mut self, what: &str, done: impl Fn(&[PlayerSessionMessage]) -> bool) {
        let deadline = Instant::now() + SCENARIO_DEADLINE;
        let first_new = self.seen.len();
        loop {
            self.media.drive(Instant::now());
            self.seen.extend(self.page.take_sent());
            if done(&self.seen[first_new..]) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(STEP);
        }
    }

    /// Крутит медиа-часть заданное время; возвращает, что увидела страница за это время.
    fn drive_for(&mut self, duration: Duration) -> Vec<PlayerSessionMessage> {
        let first_new = self.seen.len();
        let until = Instant::now() + duration;
        while Instant::now() < until {
            self.media.drive(Instant::now());
            self.seen.extend(self.page.take_sent());
            std::thread::sleep(STEP);
        }
        self.seen[first_new..].to_vec()
    }
}

/// Поток поколения: ролик фикстуры целиком (0–400 мс), длина объявлена 10 с, конца нет.
fn partially_delivered_generation(session: &SessionRef) -> (PushedStreamWriter, IntakeNotice) {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, opener) = pushed_stream(policy).expect("stream starts");
    for (lane, file) in [(0, "video.mp4"), (1, "audio.mp4")] {
        let lane = LaneId::new(lane);
        writer
            .add_lane(lane, PushedContainer::FragmentedMp4)
            .expect("lane");
        writer.append(lane, &fixture_media(file)).expect("append");
    }
    writer.set_duration(PushedDuration::Finite(DECLARED_DURATION));
    let notice = IntakeNotice::NewGeneration(Box::new(GenerationHandoff {
        session: session.clone(),
        generation: generation(1),
        source: GenerationSource::Segments {
            monitor: writer.monitor(),
            opener,
        },
    }));
    (writer, notice)
}

fn last_clock(messages: &[PlayerSessionMessage]) -> Option<(u64, ClockState)> {
    clocks(messages).last().copied()
}

fn count_seeks(messages: &[PlayerSessionMessage]) -> Vec<u64> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Seek { target_us, .. } => Some(target_us.as_micros()),
            _ => None,
        })
        .collect()
}

#[test]
fn viewer_pause_and_seeks_reach_the_real_player_and_the_page_exactly_once() {
    let mut chain = RealChain::start();
    let current = session(1);
    chain.media.on_session_opened(&current);
    let (_writer, notice) = partially_delivered_generation(&current);
    chain.media.on_intake(vec![notice], None);
    chain.drive_until("the page clock to play", |seen| {
        last_clock(seen).is_some_and(|(_, state)| state == ClockState::Playing)
    });

    // Пауза зрителя: у страницы — `pause`, часы встают на паузу (плеер её исполнил).
    chain
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, Instant::now());
    chain.drive_until("the paused clock", |seen| {
        last_clock(seen).is_some_and(|(_, state)| state == ClockState::Paused)
    });
    let pauses = chain
        .seen
        .iter()
        .filter(|message| matches!(message, PlayerSessionMessage::Pause {}))
        .count();
    assert_eq!(pauses, 1, "one pause command to the page");

    // Запоздалый `play` страницы (до её подтверждения) паузу зрителя не снимает.
    chain
        .media
        .on_intake(vec![intent(&current, PageIntent::Play)], None);
    let after_stale_play = chain.drive_for(Duration::from_millis(600));
    assert!(
        clocks(&after_stale_play)
            .iter()
            .all(|(_, state)| *state == ClockState::Paused),
        "{:?}",
        clocks(&after_stale_play)
    );

    // Перемотка на полке: плеер встаёт на цель, странице перематываться не нужно.
    chain.media.on_viewer_transport(
        ViewerTransportIntent::Seek(Duration::from_millis(200)),
        Instant::now(),
    );
    chain.drive_until("the clock at the on-shelf target", |seen| {
        last_clock(seen).is_some_and(|(position, state)| {
            state == ClockState::Paused && position.abs_diff(200_000) <= 120_000
        })
    });
    assert!(
        count_seeks(&chain.seen).is_empty(),
        "no page seek on the shelf"
    );

    // Перемотка вне полки: источник просит страницу — ровно одна просьба на действие.
    chain.media.on_viewer_transport(
        ViewerTransportIntent::Seek(Duration::from_secs(5)),
        Instant::now(),
    );
    chain.drive_until("the page seek request", |seen| {
        !count_seeks(seen).is_empty()
    });
    chain.drive_for(Duration::from_secs(1));
    assert_eq!(count_seeks(&chain.seen), vec![5_000_000]);
}
