//! Звенья строк ленты (16b2): настоящее колесо мыши и наведение, ожидания страницы и плеера.
//! Всё, что здесь измеряется, возвращается уликой для чистого приговора (`judge`), а не решается тут.

use std::time::{Duration, Instant};

use super::super::super::context::ScenarioError;
use super::super::super::entries::return_hand::ReturnHand;
use super::judge::{MIN_FRAMES_GAINED, ScrollRecord, StayRecord};
use crate::desktop_input::WheelDirection;
use crate::observe::{
    FrameGrowth, STREAM_GENERATION_LOADED_MARKER, STREAM_SURFACE_RECONFIGURED_MARKER,
    parse_stream_generation_loaded, parse_stream_surface_width,
};
use crate::site::RunId;

/// Сколько щелчков колеса в одном рывке. Рывок — малая прокрутка: лента с прилипанием сама доводит её
/// до следующей секции, а несколько рывков подряд не перескакивают через ролик.
const NOTCHES_PER_BURST: u32 = 5;
/// Сколько рывков делаем, пока страница не отреагировала (потолок, чтобы не листать бесконечно).
const MAX_BURSTS: u32 = 8;
/// Сколько ждём реакции страницы на рывок (прокрутка, наблюдатель, запуск следующего ролика), с запасом
/// на случайную паузу страницы 80-400 мс и подготовку элемента.
const BURST_SETTLE: Duration = Duration::from_secs(8);
/// Сколько страница может опаздывать с записью события колеса в журнал стенда.
const WHEEL_REPORT_GRACE: Duration = Duration::from_millis(1500);
/// Сколько ждём новую загрузку в плеере после того, как страница запустила следующий ролик: сторож
/// экстеншена, переезд, первое поколение нового потока.
const RELOCATE_TIMEOUT: Duration = Duration::from_secs(30);
/// Сколько ждём две сводки плеера с ростом кадров (сводки раз в 10 с).
const GROWTH_TIMEOUT: Duration = Duration::from_secs(60);
/// Сколько ждём раздражитель отрицательной строки на странице (превью заиграло, ролик ушёл с экрана).
const STIMULUS_TIMEOUT: Duration = Duration::from_secs(20);
/// Пауза между уходом мыши в дальний угол и приходом на превью: браузер обязан увидеть именно ВХОД.
const HOVER_ENTER_PAUSE: Duration = Duration::from_millis(400);
/// Дальний угол страницы, куда мышь уходит перед наведением (доля ширины и высоты окна).
const FAR_CORNER_SHARE: f64 = 0.95;
/// Шаг опроса журналов.
const POLL_STEP: Duration = Duration::from_millis(200);
/// Метка журнала страницы: настоящее колесо дошло до страницы.
const TRUSTED_WHEEL_MARK: &str = "wheel trusted=true";
/// Метки журнала `feed-hover`: главный ролик ушёл с экрана, не остановленный страницей / остановленный ею.
const MAIN_LEFT_UNPAUSED_MARK: &str = "main-left-screen paused=false";
const MAIN_PAUSED_ON_LEAVING_MARK: &str = "main-left-screen paused=true";
/// Метка журнала `feed-hover`: превью заиграло.
const PREVIEW_PLAYING_MARK: &str = "preview-playing";
/// Пауза между рывками колеса к комментариям: страница успевает показать превью и записать это.
const COMMENTS_BURST_PAUSE: Duration = Duration::from_secs(2);

/// Где на странице стоит курсор, когда робот крутит колесо.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum WheelPlace {
    /// В центре окна страницы: над роликом, то есть над заглушкой «Playing in Fastiplayer».
    OverReel,
    /// У левого края окна страницы: в ленте, но мимо ролика и заглушки.
    BesideReel,
}

/// Доля ширины окна страницы, где курсор стоит «мимо ролика»: ролик ленты не шире 90 % окна и стоит по
/// центру, поэтому в крайних 5 % его нет.
const BESIDE_REEL_SHARE_X: f64 = 0.025;
/// Доля ширины окна страницы для курсора «над роликом»: центр.
const OVER_REEL_SHARE_X: f64 = 0.5;

impl WheelPlace {
    /// Доля ширины окна страницы, в которой крутится колесо.
    fn share_x(self) -> f64 {
        match self {
            Self::OverReel => OVER_REEL_SHARE_X,
            Self::BesideReel => BESIDE_REEL_SHARE_X,
        }
    }
}

/// Строки журнала страницы с `needle`.
fn page_lines(hand: &ReturnHand<'_, '_>, needle: &str) -> Vec<String> {
    hand.run_ref().page_log_lines(needle)
}

/// Номера поколений потока, которые плеер загрузил за всё время (по порядку журнала).
pub(super) fn loaded_generations(hand: &mut ReturnHand<'_, '_>) -> Result<Vec<u64>, ScenarioError> {
    let lines = hand
        .run()
        .player_log
        .snapshot_lines_containing(STREAM_GENERATION_LOADED_MARKER)?;
    Ok(lines
        .iter()
        .filter_map(|line| parse_stream_generation_loaded(line))
        .collect())
}

/// Ширины кадра потоков, которые окно показывало за всё время (строки «пул поверхностей перестроен под
/// поток», по порядку журнала): по ним видно, какой из роликов ленты сейчас в окне.
pub(super) fn shown_stream_widths(
    hand: &mut ReturnHand<'_, '_>,
) -> Result<Vec<u64>, ScenarioError> {
    let lines = hand
        .run()
        .player_log
        .snapshot_lines_containing(STREAM_SURFACE_RECONFIGURED_MARKER)?;
    Ok(lines
        .iter()
        .filter_map(|line| parse_stream_surface_width(line))
        .collect())
}

/// Один рывок настоящего колеса на середине высоты видимой части страницы, по ширине — где велит
/// `wheel`. Перед каждым рывком браузер выводится вперёд и страница заново калибруется: окно плеера
/// после переезда могло снова оказаться сверху.
fn wheel_burst(
    hand: &ReturnHand<'_, '_>,
    run: &RunId,
    wheel: WheelPlace,
) -> Result<(), ScenarioError> {
    let place = hand.locate(run)?;
    let viewport = place.target_rect("viewport").ok_or_else(|| {
        ScenarioError::EntryNotActivated("the page reported no viewport to scroll in".into())
    })?;
    let point = place.screen_point_in(viewport, wheel.share_x(), 0.5);
    hand.scroll_wheel(point, WheelDirection::Down, NOTCHES_PER_BURST)
}

/// Рост кадров между двумя сводками, начиная с НОВЫХ строк журнала (старые сводки не в счёт).
fn frame_growth_from_now(hand: &mut ReturnHand<'_, '_>) -> Result<FrameGrowth, String> {
    let log = &mut hand.run().player_log;
    log.skip_history().map_err(|error| error.to_string())?;
    log.wait_for_frame_growth(MIN_FRAMES_GAINED, GROWTH_TIMEOUT)
        .map_err(|error| error.to_string())
}

/// Ждёт, пока число загрузок поколений в плеере станет больше `before`.
fn wait_for_new_generation(hand: &mut ReturnHand<'_, '_>, before: usize) -> Result<(), String> {
    let deadline = Instant::now() + RELOCATE_TIMEOUT;
    loop {
        let seen = loaded_generations(hand)
            .map_err(|error| error.to_string())?
            .len();
        if seen > before {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the player loaded no new generation within {RELOCATE_TIMEOUT:?} (still {seen} load(s))"
            ));
        }
        std::thread::sleep(POLL_STEP);
    }
}

/// Листает ленту колесом до ролика `target_reel` и собирает улики: дошло ли колесо, запустила ли страница
/// следующий ролик, переехало ли окно, растут ли кадры.
pub(super) fn scroll_to_reel(
    hand: &mut ReturnHand<'_, '_>,
    target_reel: u32,
    wheel: WheelPlace,
) -> Result<ScrollRecord, ScenarioError> {
    let run = hand.run_ref().run.clone();
    let started_mark = format!("reel-started n={target_reel}");
    let wheel_before = page_lines(hand, TRUSTED_WHEEL_MARK).len();
    let generations_before = loaded_generations(hand)?.len();
    let mut page_started_next_reel = Err("no wheel burst was made".to_owned());
    for burst in 1..=MAX_BURSTS {
        wheel_burst(hand, &run, wheel)?;
        if page_lines(hand, TRUSTED_WHEEL_MARK).len() == wheel_before {
            // Страница могла записать событие с опозданием: даём ей время, прежде чем сказать «не дошло».
            std::thread::sleep(WHEEL_REPORT_GRACE);
            if page_lines(hand, TRUSTED_WHEEL_MARK).len() == wheel_before {
                page_started_next_reel = Err(format!(
                    "no wheel event reached the page after burst {burst}"
                ));
                break;
            }
        }
        match hand
            .run_ref()
            .wait_page_log_line(&started_mark, BURST_SETTLE)
        {
            Ok(_) => {
                page_started_next_reel = Ok(());
                break;
            }
            Err(message) => {
                page_started_next_reel = Err(format!("after {burst} burst(s): {message}"));
            }
        }
    }
    let trusted_wheel_events = page_lines(hand, TRUSTED_WHEEL_MARK).len() - wheel_before;
    let (player_loaded_new_generation, frame_growth) = if page_started_next_reel.is_ok() {
        let loaded = wait_for_new_generation(hand, generations_before);
        let growth = match &loaded {
            Ok(()) => frame_growth_from_now(hand),
            Err(_) => Err("not awaited: the window did not relocate".to_owned()),
        };
        (loaded, growth)
    } else {
        let skipped = "not awaited: the page did not start the next reel".to_owned();
        (Err(skipped.clone()), Err(skipped))
    };
    Ok(ScrollRecord {
        trusted_wheel_events,
        page_started_next_reel,
        player_loaded_new_generation,
        frame_growth,
    })
}

/// Ждёт в журнале страницы все `needles`; `Err` называет, чего не дождались.
fn wait_for_page_facts(hand: &ReturnHand<'_, '_>, needles: &[&str]) -> Result<(), String> {
    for needle in needles {
        hand.run_ref()
            .wait_page_log_line(needle, STIMULUS_TIMEOUT)
            .map_err(|message| format!("the page never logged '{needle}': {message}"))?;
    }
    Ok(())
}

/// Отрицательная строка «превью по наведению»: настоящая мышь приходит на превью рядом с главным видео.
pub(super) fn hover_over_preview(
    hand: &mut ReturnHand<'_, '_>,
) -> Result<StayRecord, ScenarioError> {
    let run = hand.run_ref().run.clone();
    let place = hand.locate(&run)?;
    let missing =
        |what: &str| ScenarioError::EntryNotActivated(format!("the page reported no {what}"));
    let preview = place
        .target_rect("preview")
        .ok_or_else(|| missing("'preview' target"))?;
    let viewport = place
        .target_rect("viewport")
        .ok_or_else(|| missing("viewport"))?;
    let far = place.screen_point_in(viewport, FAR_CORNER_SHARE, FAR_CORNER_SHARE);
    let over = place.screen_point_in(preview, 0.5, 0.5);
    hand.move_pointer(far)?;
    std::thread::sleep(HOVER_ENTER_PAUSE);
    hand.move_pointer(over)?;
    let stimulus = wait_for_page_facts(
        hand,
        &[
            "preview-start-trigger why=hover",
            "preview-playing muted=true",
        ],
    );
    Ok(StayRecord {
        stimulus,
        frame_growth: frame_growth_from_now(hand),
    })
}

/// Отрицательная строка «листаем к комментариям»: колесом главный ролик уходит с экрана (страница его НЕ
/// останавливает), ниже заиграло превью.
pub(super) fn scroll_to_comments(
    hand: &mut ReturnHand<'_, '_>,
) -> Result<StayRecord, ScenarioError> {
    let run = hand.run_ref().run.clone();
    let mut stimulus = Err("no wheel burst was made".to_owned());
    for burst in 1..=MAX_BURSTS {
        wheel_burst(hand, &run, WheelPlace::OverReel)?;
        if !page_lines(hand, MAIN_PAUSED_ON_LEAVING_MARK).is_empty() {
            // Страница сама остановила главный ролик: это уже не случай «листаем к комментариям».
            stimulus = Err("the page paused the main video when it left the screen".to_owned());
            break;
        }
        if !page_lines(hand, MAIN_LEFT_UNPAUSED_MARK).is_empty()
            && !page_lines(hand, PREVIEW_PLAYING_MARK).is_empty()
        {
            stimulus = Ok(());
            break;
        }
        stimulus = Err(format!(
            "after {burst} burst(s) the main video had not left the screen with the preview playing"
        ));
        std::thread::sleep(COMMENTS_BURST_PAUSE);
    }
    Ok(StayRecord {
        stimulus,
        frame_growth: frame_growth_from_now(hand),
    })
}
