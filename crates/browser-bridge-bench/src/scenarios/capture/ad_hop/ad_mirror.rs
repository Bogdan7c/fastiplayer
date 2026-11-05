//! Плашка рекламы в окне плеера (сессия 09d, решения 184, 206): робот видит в журнале плеера то, что
//! видит зритель, и жмёт пропуск публичной командой MPRIS `Next`, как медиаклавиша. Служебных
//! входов в плеер нет: цепочка та же, что у живого зрителя, кроме самого клика по кнопке.
//!
//! Страница `ad-skip` держит органы в форме PlayerJS, настоящий читатель экстеншена их читает.

use std::time::{Duration, Instant};

use super::super::super::handoff_run::HandoffRun;
use crate::environment::BenchEnvironment;
use crate::player_control::PlayerControl;

/// Что робот делает с плашкой.
#[derive(Debug, Clone, Copy)]
pub(in crate::scenarios::capture) enum AdMirrorScript {
    /// Пропустить каждую рекламу блока из окна плеера.
    SkipEachAd {
        /// Сколько реклам в блоке.
        ads: u16,
    },
    /// Реклама непропускаемая: кнопка не должна появиться ни разу.
    SkipNeverOffered,
    /// Страница показала кнопку, но наш клик отвергает: реклама доигрывает сама.
    PageRefusesOurClick,
}

/// Строка журнала плеера о плашке с кнопкой пропуска.
const SKIP_OFFERED: &str = "skip=skip";
/// Строка журнала плеера: плашка снята.
const PLATE_CLEARED: &str = "Ad plate cleared";
/// Метка страницы: наш клик отвергнут.
const CLICK_REFUSED: &str = "skip-click-refused";
/// Сколько ждём плашку: передача, открытие окна и первое чтение органов.
const PLATE_TIMEOUT: Duration = Duration::from_secs(45);
/// Сколько ждём кнопку: отсчёт страницы 3 с плюс запас на медленный старт ролика.
const SKIP_OFFER_TIMEOUT: Duration = Duration::from_secs(30);
/// Сколько пробуем нажать: `CanGoNext` окно публикует чуть позже строки журнала.
const PRESS_TIMEOUT: Duration = Duration::from_secs(10);
/// Пауза между попытками нажать: защита «кнопка в пути» лишнюю попытку всё равно отсечёт.
const PRESS_RETRY_STEP: Duration = Duration::from_millis(400);
/// Сколько ждём снятие плашки: фон считает молчание фрейма концом рекламы за 3–4 с.
const CLEARED_TIMEOUT: Duration = Duration::from_secs(20);
/// Попытка перемотки в рекламе: дальше конца первого ролика.
const FORBIDDEN_SEEK_MICROS: i64 = 30_000_000;
/// Сколько ждём, чтобы запрещённая перемотка успела сработать, если запрет сломан.
const SEEK_SETTLE: Duration = Duration::from_millis(600);
/// На сколько позиция может уйти за это время сама.
const NATURAL_ADVANCE_MICROS: i64 = 3_000_000;

/// Пройти сценарий плашки до снятия плашки. `Err` — звено цепи не сработало.
pub(in crate::scenarios::capture) fn follow_the_ad_mirror(
    run: &mut HandoffRun<'_>,
    environment: &BenchEnvironment,
    script: AdMirrorScript,
) -> Result<(), String> {
    let control = PlayerControl::on_bus(
        environment
            .bus_address()
            .map_err(|error| format!("no player bus: {error}"))?,
    );
    control
        .wait_until_controllable(PLATE_TIMEOUT)
        .map_err(|error| format!("the player never became controllable: {error}"))?;
    match script {
        AdMirrorScript::SkipEachAd { ads } => {
            for index in 1..=ads {
                let plate = plate_needle(index, ads);
                wait_plate(run, &plate, "the numbered ad plate")?;
                if index == 1 {
                    refuse_seek_during_the_ad(&control)?;
                }
                wait_plate(run, SKIP_OFFERED, "the Skip button")?;
                press_until_page_reacts(run, &control, &format!("skip-pressed ad{index}"))?;
            }
        }
        AdMirrorScript::SkipNeverOffered => {
            wait_plate(run, &plate_needle(1, 1), "the numbered ad plate")?;
        }
        AdMirrorScript::PageRefusesOurClick => {
            wait_plate(run, SKIP_OFFERED, "the Skip button")?;
            press_until_page_reacts(run, &control, CLICK_REFUSED)?;
        }
    }
    wait_plate_with(run, PLATE_CLEARED, "the plate to go away", CLEARED_TIMEOUT)?;
    if matches!(script, AdMirrorScript::SkipNeverOffered) {
        let offers = run
            .player_log
            .snapshot_forbidden(&[SKIP_OFFERED])
            .map_err(|error| format!("cannot read the player log: {error}"))?;
        if let Some(line) = offers.first() {
            return Err(format!(
                "the plate offered Skip on an unskippable ad: {line}"
            ));
        }
    }
    Ok(())
}

/// Подстрока журнала о плашке с этим счётчиком.
fn plate_needle(index: u16, count: u16) -> String {
    format!("ad_index={index} ad_count={count}")
}

fn wait_plate(run: &mut HandoffRun<'_>, needle: &str, what: &str) -> Result<(), String> {
    let timeout = if needle == SKIP_OFFERED {
        SKIP_OFFER_TIMEOUT
    } else {
        PLATE_TIMEOUT
    };
    wait_plate_with(run, needle, what, timeout)
}

fn wait_plate_with(
    run: &mut HandoffRun<'_>,
    needle: &str,
    what: &str,
    timeout: Duration,
) -> Result<(), String> {
    run.player_log
        .wait_for_line(needle, timeout)
        .map(|_| ())
        .map_err(|error| format!("the player never showed {what}: {error}"))
}

/// Решение 206: перемотка из окна в рекламе не двигает ролик. Сама команда может быть отвергнута
/// шиной (`CanSeek=false`): важен результат, а не ответ.
fn refuse_seek_during_the_ad(control: &PlayerControl) -> Result<(), String> {
    let before = control
        .position_micros()
        .map_err(|error| format!("cannot read the player position: {error}"))?;
    let _ignored_bus_answer = control.seek_by(FORBIDDEN_SEEK_MICROS);
    std::thread::sleep(SEEK_SETTLE);
    let after = control
        .position_micros()
        .map_err(|error| format!("cannot read the player position: {error}"))?;
    if after - before > NATURAL_ADVANCE_MICROS {
        return Err(format!(
            "the viewer rewound an ad: position jumped {before} -> {after} us"
        ));
    }
    Ok(())
}

/// Жмём `Next`, пока страница не отзовётся меткой. Повтор нужен только из-за задержки `CanGoNext`.
fn press_until_page_reacts(
    run: &HandoffRun<'_>,
    control: &PlayerControl,
    page_marker: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + PRESS_TIMEOUT;
    let mut last_failure = String::from("no attempt");
    while Instant::now() < deadline {
        let pressed = control.next();
        match run.wait_page_log_line(page_marker, PRESS_RETRY_STEP) {
            Ok(_) => return Ok(()),
            Err(message) => {
                last_failure = match pressed {
                    Ok(()) => message,
                    Err(error) => format!("{error}; {message}"),
                };
            }
        }
    }
    Err(format!(
        "the page never reacted to Skip with '{page_marker}': {last_failure}"
    ))
}
