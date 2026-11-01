//! Претензия клика зрителя (решение 149): чистые правила, без медиа-части.

use std::time::{Duration, Instant};

use super::{PageSeekAgainstClick, ViewerClickClaim};
use crate::browser_session::session_media::viewer_transport::VIEWER_WILL_CONFIRMATION_WINDOW;

const CLICK: Duration = Duration::from_secs(300);
const PAGE_RETURN: Duration = Duration::from_secs(120);
/// Допуск совпадения источника (250 мс).
const TOLERANCE: Duration = Duration::from_millis(250);

fn same(first: Duration, second: Duration) -> bool {
    first.abs_diff(second) <= TOLERANCE
}

/// Без клика во время смены поколения своя перемотка страницы — обычная.
#[test]
fn without_a_click_a_page_seek_is_unclaimed() {
    let mut claim = ViewerClickClaim::default();
    assert_eq!(
        claim.judge_page_seek(PAGE_RETURN, same),
        PageSeekAgainstClick::Unclaimed
    );
    assert_eq!(claim.take_repeat_when_page_ready(Instant::now()), None);
}

/// Возврат страницы в другое место — долг повтора; повтор один, при показе поколения.
#[test]
fn a_page_return_elsewhere_owes_exactly_one_repeat_when_the_page_is_ready() {
    let mut claim = ViewerClickClaim::default();
    claim.arm(CLICK);
    assert_eq!(
        claim.judge_page_seek(PAGE_RETURN, same),
        PageSeekAgainstClick::Contradicts { target: CLICK }
    );
    let now = Instant::now();
    assert_eq!(claim.take_repeat_when_page_ready(now), Some(CLICK));
    // Повтор израсходован: страница не подчинилась и со второго раза — без третьего.
    assert_eq!(
        claim.judge_page_seek(PAGE_RETURN, same),
        PageSeekAgainstClick::Unclaimed
    );
    assert_eq!(claim.take_repeat_when_page_ready(now), None);
    assert_eq!(claim.take_owed_repeat(), None);
}

/// Эхо клика (в допуске) — не возврат: повтора нет.
#[test]
fn an_echo_of_the_click_owes_nothing() {
    let mut claim = ViewerClickClaim::default();
    claim.arm(CLICK);
    assert_eq!(
        claim.judge_page_seek(CLICK + Duration::from_millis(200), same),
        PageSeekAgainstClick::EchoOfClick
    );
    assert_eq!(claim.take_repeat_when_page_ready(Instant::now()), None);
}

/// Страница сначала вернулась, но до показа всё же пришла к клику: долг снят.
#[test]
fn a_page_that_comes_to_the_click_after_returning_owes_no_repeat() {
    let mut claim = ViewerClickClaim::default();
    claim.arm(CLICK);
    claim.judge_page_seek(PAGE_RETURN, same);
    assert_eq!(
        claim.judge_page_seek(CLICK, same),
        PageSeekAgainstClick::EchoOfClick
    );
    assert_eq!(claim.take_repeat_when_page_ready(Instant::now()), None);
}

/// После показа поколения возврат страницы в окне подтверждения — повтор сразу;
/// после окна — страница снова ведёт сама (реклама, автопереход).
#[test]
fn after_the_page_is_ready_the_claim_lasts_only_the_confirmation_window() {
    let shown_at = Instant::now();
    let mut in_window = ViewerClickClaim::default();
    in_window.arm(CLICK);
    assert_eq!(in_window.take_repeat_when_page_ready(shown_at), None);
    in_window.expire(shown_at + VIEWER_WILL_CONFIRMATION_WINDOW - Duration::from_millis(1));
    assert_eq!(
        in_window.judge_page_seek(PAGE_RETURN, same),
        PageSeekAgainstClick::Contradicts { target: CLICK }
    );
    assert_eq!(in_window.take_owed_repeat(), Some(CLICK));

    let mut expired = ViewerClickClaim::default();
    expired.arm(CLICK);
    expired.take_repeat_when_page_ready(shown_at);
    expired.expire(shown_at + VIEWER_WILL_CONFIRMATION_WINDOW);
    assert_eq!(
        expired.judge_page_seek(PAGE_RETURN, same),
        PageSeekAgainstClick::Unclaimed
    );
}

/// Новое действие зрителя заменяет претензию; клик без смены поколения её снимает.
#[test]
fn a_new_viewer_action_replaces_or_clears_the_claim() {
    let mut claim = ViewerClickClaim::default();
    claim.arm(CLICK);
    claim.arm(Duration::from_secs(500));
    assert_eq!(
        claim.judge_page_seek(CLICK, same),
        PageSeekAgainstClick::Contradicts {
            target: Duration::from_secs(500)
        }
    );
    claim.forget();
    assert_eq!(
        claim.judge_page_seek(PAGE_RETURN, same),
        PageSeekAgainstClick::Unclaimed
    );
}

/// Находка ревью: эхо клика ПОСЛЕ показа не сбрасывает окно подтверждения — окно
/// кончается, претензии нет, и поздняя своя перемотка страницы (глава, реклама) просто
/// исполняется, без повтора старого клика.
#[test]
fn an_echo_after_the_show_keeps_the_confirmation_window_running() {
    let shown_at = Instant::now();
    let mut claim = ViewerClickClaim::default();
    claim.arm(CLICK);
    assert_eq!(claim.take_repeat_when_page_ready(shown_at), None);
    assert_eq!(
        claim.judge_page_seek(CLICK + Duration::from_millis(100), same),
        PageSeekAgainstClick::EchoOfClick
    );
    claim.expire(shown_at + VIEWER_WILL_CONFIRMATION_WINDOW);
    assert_eq!(
        claim.judge_page_seek(PAGE_RETURN, same),
        PageSeekAgainstClick::Unclaimed
    );
}
