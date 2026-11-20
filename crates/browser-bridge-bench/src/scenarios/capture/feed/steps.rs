//! Звенья строк ленты (16b2): настоящее колесо мыши и наведение, ожидания страницы и плеера.
//! Всё, что здесь измеряется, возвращается уликой для чистого приговора (`judge`), а не решается тут.

use std::time::{Duration, Instant};

use super::super::super::context::ScenarioError;
use super::super::super::entries::return_hand::{PagePlace, ReturnHand};
use super::judge::{FrameGrowthCheck, MIN_FRAMES_GAINED, ScrollRecord, StayRecord};
use crate::desktop_input::{Point, WheelDirection};
use crate::observe::{
    FrameGrowth, FrameGrowthRule, PLAYBACK_GROWTH_WINDOW, STREAM_GENERATION_LOADED_MARKER,
    STREAM_SURFACE_RECONFIGURED_MARKER, parse_stream_generation_loaded, parse_stream_surface_width,
};
use crate::site::RunId;

/// Сколько щелчков колеса в одном рывке. Рывок — малая прокрутка: лента с прилипанием сама доводит её
/// до следующей секции, а несколько рывков подряд не перескакивают через ролик.
pub(super) const NOTCHES_PER_BURST: u32 = 5;
/// Сколько рывков делаем, пока страница не отреагировала (потолок, чтобы не листать бесконечно).
const MAX_BURSTS: u32 = 8;
/// Сколько ждём реакции страницы на рывок (прокрутка, наблюдатель, запуск следующего ролика), с запасом
/// на случайную паузу страницы 80-400 мс и подготовку элемента.
pub(super) const BURST_SETTLE: Duration = Duration::from_secs(8);
/// Сколько страница может опаздывать с записью события колеса в журнал стенда.
const WHEEL_REPORT_GRACE: Duration = Duration::from_millis(1500);
/// Сколько ждём новую загрузку в плеере после того, как страница запустила следующий ролик: сторож
/// экстеншена, переезд, первое поколение нового потока.
pub(super) const RELOCATE_TIMEOUT: Duration = Duration::from_secs(30);
/// Сколько ждём две сводки плеера с ростом кадров (сводки раз в 10 с).
const GROWTH_TIMEOUT: Duration = Duration::from_secs(60);
/// Сколько ждём раздражитель отрицательной строки на странице (превью заиграло, ролик ушёл с экрана).
pub(super) const STIMULUS_TIMEOUT: Duration = Duration::from_secs(20);
/// Пауза между уходом мыши в дальний угол и приходом на превью: браузер обязан увидеть именно ВХОД.
const HOVER_ENTER_PAUSE: Duration = Duration::from_millis(400);
/// Дальний угол страницы, куда мышь уходит перед наведением (доля ширины и высоты окна).
const FAR_CORNER_SHARE: f64 = 0.95;
/// Шаг опроса журналов.
pub(super) const POLL_STEP: Duration = Duration::from_millis(200);
/// Метка журнала страницы: настоящее колесо дошло до страницы.
const TRUSTED_WHEEL_MARK: &str = "wheel trusted=true";
/// Нагрузка страницы перед прокруткой: команда-хук и отметка журнала страницы о том, что нагрузка была.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) struct PageLoad {
    /// Команда странице (`hook:<имя>`).
    pub command: &'static str,
    /// Начало строки журнала страницы, которую она пишет в конце нагрузки.
    pub finished_mark: &'static str,
}

/// Скачать больше, чем помнит мост (хук `download-past-bridge-memory` страницы `feed`).
pub(in super::super) const DOWNLOAD_PAST_BRIDGE_MEMORY: PageLoad = PageLoad {
    command: "hook:download-past-bridge-memory",
    finished_mark: "earlier-reel-downloaded",
};
/// Сбросить пройденные ролики и скачать начала дальних сверх памяти моста (хук
/// `detach-passed-reels-and-preload-far-ahead`). Отметка ждётся вместе с числами формы: все три пройденных
/// ролика закрыты, все восемь дальних живы. Иная форма страницы строку не доказывает.
pub(in super::super) const DETACH_PASSED_REELS_AND_PRELOAD_FAR_AHEAD: PageLoad = PageLoad {
    command: "hook:detach-passed-reels-and-preload-far-ahead",
    finished_mark: "far-reels-preloaded passed_closed=3 far_open=8 mib=",
};
/// Сколько ждём докачку: 80 МиБ кусками по мебибайту идут секунды, запас — на медленный браузер стенда.
const PAGE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(90);
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
pub(super) fn page_lines(hand: &ReturnHand<'_, '_>, needle: &str) -> Vec<String> {
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

/// Точка экрана на середине высоты видимой части страницы, по ширине — где велит `wheel`. Перед этим
/// браузер выводится вперёд и страница заново калибруется: окно плеера после переезда могло снова оказаться
/// сверху.
pub(super) fn wheel_point(
    hand: &ReturnHand<'_, '_>,
    run: &RunId,
    wheel: WheelPlace,
) -> Result<Point, ScenarioError> {
    let place = locate_with_retries(hand, run)?;
    let viewport = place.target_rect("viewport").ok_or_else(|| {
        ScenarioError::EntryNotActivated("the page reported no viewport to scroll in".into())
    })?;
    Ok(place.screen_point_in(viewport, wheel.share_x(), 0.5))
}

/// Сколько раз пробуем калибровку экрана, прежде чем сдаться. Две точки калибровки иногда расходятся, пока
/// окно браузера ещё встаёт на место после передачи (на стенде: «the two calibration points disagree»).
const LOCATE_ATTEMPTS: u32 = 3;
/// Пауза между попытками калибровки: окну нужно время встать на место.
const LOCATE_RETRY_PAUSE: Duration = Duration::from_millis(700);

/// Калибровка страницы с повтором: последняя ошибка возвращается как есть, не глотается.
fn locate_with_retries(hand: &ReturnHand<'_, '_>, run: &RunId) -> Result<PagePlace, ScenarioError> {
    let mut attempt = 1;
    loop {
        match hand.locate(run) {
            Ok(place) => return Ok(place),
            Err(error) if attempt >= LOCATE_ATTEMPTS => return Err(error),
            Err(_) => {
                attempt += 1;
                std::thread::sleep(LOCATE_RETRY_PAUSE);
            }
        }
    }
}

/// Один рывок настоящего колеса в точке [`wheel_point`], в сторону `direction`.
fn wheel_burst(
    hand: &ReturnHand<'_, '_>,
    run: &RunId,
    wheel: WheelPlace,
    direction: WheelDirection,
) -> Result<(), ScenarioError> {
    let point = wheel_point(hand, run, wheel)?;
    hand.scroll_wheel(point, direction, NOTCHES_PER_BURST)
}

/// Рост кадров между двумя сводками, начиная с НОВЫХ строк журнала (старые сводки не в счёт).
pub(super) fn frame_growth_from_now(hand: &mut ReturnHand<'_, '_>) -> Result<FrameGrowth, String> {
    let log = &mut hand.run().player_log;
    log.skip_history().map_err(|error| error.to_string())?;
    log.wait_for_frame_growth(
        FrameGrowthRule::frames_within(MIN_FRAMES_GAINED, PLAYBACK_GROWTH_WINDOW),
        GROWTH_TIMEOUT,
    )
    .map_err(|error| error.to_string())
}

/// Ждёт, пока число загрузок поколений в плеере станет больше `before`.
pub(super) fn wait_for_new_generation(
    hand: &mut ReturnHand<'_, '_>,
    before: usize,
) -> Result<(), String> {
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

/// Итог рывков колеса до метки журнала страницы.
pub(super) struct WheelOutcome {
    /// Сколько настоящих событий колеса (`wheel trusted=true`) страница получила за рывки.
    pub trusted_wheel_events: usize,
    /// Страница записала нужную метку или причина, почему нет.
    pub reached: Result<(), String>,
}

/// Крутит настоящее колесо ВНИЗ рывками, пока страница не запишет `mark` (потолок — `MAX_BURSTS` рывков).
pub(super) fn wheel_until_page_logs(
    hand: &mut ReturnHand<'_, '_>,
    mark: &str,
    wheel: WheelPlace,
) -> Result<WheelOutcome, ScenarioError> {
    wheel_until_page_logs_toward(hand, mark, wheel, WheelDirection::Down)
}

/// То же, что [`wheel_until_page_logs`], но колесо крутится в сторону `direction` (вверх — прокрутка
/// ленты назад, 16b3b).
pub(super) fn wheel_until_page_logs_toward(
    hand: &mut ReturnHand<'_, '_>,
    mark: &str,
    wheel: WheelPlace,
    direction: WheelDirection,
) -> Result<WheelOutcome, ScenarioError> {
    let run = hand.run_ref().run.clone();
    let wheel_before = page_lines(hand, TRUSTED_WHEEL_MARK).len();
    let mut reached = Err("no wheel burst was made".to_owned());
    for burst in 1..=MAX_BURSTS {
        wheel_burst(hand, &run, wheel, direction)?;
        if page_lines(hand, TRUSTED_WHEEL_MARK).len() == wheel_before {
            // Страница могла записать событие с опозданием: даём ей время, прежде чем сказать «не дошло».
            std::thread::sleep(WHEEL_REPORT_GRACE);
            if page_lines(hand, TRUSTED_WHEEL_MARK).len() == wheel_before {
                reached = Err(format!(
                    "no wheel event reached the page after burst {burst}"
                ));
                break;
            }
        }
        match hand.run_ref().wait_page_log_line(mark, BURST_SETTLE) {
            Ok(_) => {
                reached = Ok(());
                break;
            }
            Err(message) => {
                reached = Err(format!("after {burst} burst(s): {message}"));
            }
        }
    }
    Ok(WheelOutcome {
        trusted_wheel_events: page_lines(hand, TRUSTED_WHEEL_MARK).len() - wheel_before,
        reached,
    })
}

/// Листает ленту колесом до ролика `target_reel` и собирает улики: дошло ли колесо, запустила ли страница
/// следующий ролик, переехало ли окно, растут ли кадры.
pub(super) fn scroll_to_reel(
    hand: &mut ReturnHand<'_, '_>,
    target_reel: u32,
    wheel: WheelPlace,
) -> Result<ScrollRecord, ScenarioError> {
    scroll_to_reel_then(hand, target_reel, wheel, FrameGrowthCheck::Required, |_| {
        Ok(())
    })
}

/// То же, что [`scroll_to_reel`], но `after_relocation` исполняется сразу после того, как окно загрузило
/// новое поколение, и до ожидания роста кадров (строка «страница убирает элемент, на который переехало
/// окно» обязана ударить в первую секунду после переезда). С `FrameGrowthCheck::NotRequired` рост кадров
/// не ждётся вовсе: он не судится, а ожидание стоило бы десятков секунд до следующей прокрутки.
pub(super) fn scroll_to_reel_then(
    hand: &mut ReturnHand<'_, '_>,
    target_reel: u32,
    wheel: WheelPlace,
    frame_growth_check: FrameGrowthCheck,
    after_relocation: impl FnOnce(&mut ReturnHand<'_, '_>) -> Result<(), ScenarioError>,
) -> Result<ScrollRecord, ScenarioError> {
    let generations_before = loaded_generations(hand)?.len();
    let started_mark = format!("reel-started n={target_reel}");
    let WheelOutcome {
        trusted_wheel_events,
        reached: page_started_next_reel,
    } = wheel_until_page_logs(hand, &started_mark, wheel)?;
    let (player_loaded_new_generation, frame_growth) = if page_started_next_reel.is_ok() {
        let loaded = wait_for_new_generation(hand, generations_before);
        if loaded.is_ok() {
            after_relocation(hand)?;
        }
        let growth = match (&loaded, frame_growth_check) {
            (Ok(()), FrameGrowthCheck::Required) => frame_growth_from_now(hand),
            (Ok(()), FrameGrowthCheck::NotRequired) => {
                Err("not awaited: the row does not judge it".to_owned())
            }
            (Err(_), _) => Err("not awaited: the window did not relocate".to_owned()),
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
        frame_growth_check,
    })
}

/// Страница качает больше, чем помнит мост (`site/lib/feed-history.js`): робот запускает нагрузку хуком и
/// ждёт отметку страницы. `Err` называет, чего не дождались.
pub(super) fn run_page_load(hand: &mut ReturnHand<'_, '_>, load: PageLoad) -> Result<(), String> {
    hand.run().tell_page(load.command);
    hand.run_ref()
        .wait_page_log_line(load.finished_mark, PAGE_DOWNLOAD_TIMEOUT)
        .map(|_| ())
        .map_err(|message| format!("the page never logged '{}': {message}", load.finished_mark))
}

/// Ждёт в журнале страницы все `needles`; `Err` называет, чего не дождались.
pub(super) fn wait_for_page_facts(
    hand: &ReturnHand<'_, '_>,
    needles: &[&str],
) -> Result<(), String> {
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
        wheel_burst(hand, &run, WheelPlace::OverReel, WheelDirection::Down)?;
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
