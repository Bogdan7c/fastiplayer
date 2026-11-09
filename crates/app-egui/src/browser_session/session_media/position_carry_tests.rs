//! Правила возврата на секунду (решение 280) без медиа-части: чистое состояние
//! [`PositionCarry`]. Сквозь `SessionMedia` с настоящим потоком — `tests/position_carry.rs`.

use super::*;
use crate::browser_session::playback::MediaLoadTicketIssuer;
use crate::browser_session::playback::track_layout::TrackLayoutStamp;

const TARGET: Duration = Duration::from_secs(600);
const LIFETIME: Duration = Duration::from_secs(120);

fn generation(number: u32) -> StreamGeneration {
    StreamGeneration::new(number).expect("generation")
}

fn armed_carry(now: Instant, origin_tracks: ShownTrackLayout) -> PositionCarry {
    let mut carry = PositionCarry::new(PositionCarryPolicy::new(LIFETIME));
    carry.arm(TARGET, now, origin_tracks);
    carry
}

/// Возврат, отданный поколению 2, которому уже ушла просьба странице.
fn placed_carry(now: Instant) -> PositionCarry {
    let mut carry = armed_carry(now, ShownTrackLayout::Unknown);
    assert_eq!(
        carry.on_new_generation(generation(2), now, AdOnScreen::No),
        NewGenerationCarry::PlaceAt(TARGET)
    );
    carry.note_page_asked();
    carry
}

fn exact(first: Duration, second: Duration) -> bool {
    first == second
}

fn view(shown: PlacedGenerationShown, ad: AdOnScreen) -> CarrySettleView {
    CarrySettleView {
        placed_generation_shown: shown,
        ad_on_screen: ad,
        shown_tracks: ShownTrackLayout::Unknown,
    }
}

#[test]
fn without_a_selection_no_generation_is_carried() {
    let mut carry = PositionCarry::new(PositionCarryPolicy::new(LIFETIME));
    assert_eq!(
        carry.on_new_generation(generation(2), Instant::now(), AdOnScreen::No),
        NewGenerationCarry::NotCarried
    );
    assert_eq!(carry.target(), None);
}

#[test]
fn an_ad_generation_is_skipped_and_the_film_after_it_gets_the_position() {
    let now = Instant::now();
    let mut carry = armed_carry(now, ShownTrackLayout::Unknown);
    assert_eq!(
        carry.on_new_generation(generation(2), now, AdOnScreen::Yes),
        NewGenerationCarry::SkippedAd
    );
    assert_eq!(carry.target(), Some(TARGET), "the carry survives the ad");
    let later = now + Duration::from_secs(35);
    assert_eq!(
        carry.on_new_generation(generation(3), later, AdOnScreen::No),
        NewGenerationCarry::PlaceAt(TARGET)
    );
    assert_eq!(carry.placed_generation(), Some(generation(3)));
}

#[test]
fn the_carry_expires_and_a_late_generation_starts_where_the_page_says() {
    let now = Instant::now();
    let mut carry = armed_carry(now, ShownTrackLayout::Unknown);
    assert_eq!(
        carry.on_new_generation(generation(2), now + LIFETIME, AdOnScreen::No),
        NewGenerationCarry::NotCarried
    );
    assert_eq!(carry.target(), None);

    let mut waiting = armed_carry(now, ShownTrackLayout::Unknown);
    assert_eq!(
        waiting.settle(
            now + LIFETIME - Duration::from_millis(1),
            view(PlacedGenerationShown::NotShown, AdOnScreen::No)
        ),
        CarrySettle::Nothing
    );
    assert_eq!(
        waiting.settle(
            now + LIFETIME,
            view(PlacedGenerationShown::NotShown, AdOnScreen::No)
        ),
        CarrySettle::Ended(CarryEnd::Expired)
    );
}

#[test]
fn a_new_track_description_of_the_same_shown_media_ends_the_carry() {
    let now = Instant::now();
    let mut tickets = MediaLoadTicketIssuer::default();
    let shown = tickets.issue();
    let origin = ShownTrackLayout::Shown {
        load: shown,
        stamp: TrackLayoutStamp::for_test(1),
    };
    let mut carry = armed_carry(now, origin);
    // Тот же показ, то же описание: смены нет.
    let same = CarrySettleView {
        shown_tracks: origin,
        ..view(PlacedGenerationShown::NotShown, AdOnScreen::No)
    };
    assert_eq!(carry.settle(now, same), CarrySettle::Nothing);
    // Другое медиа (новая загрузка) с другим описанием — не смена в том же поколении.
    let other_load = CarrySettleView {
        shown_tracks: ShownTrackLayout::Shown {
            load: tickets.issue(),
            stamp: TrackLayoutStamp::for_test(2),
        },
        ..same
    };
    assert_eq!(carry.settle(now, other_load), CarrySettle::Nothing);
    // Эстафета 03d: то же медиа, новое описание дорожек.
    let relayed = CarrySettleView {
        shown_tracks: ShownTrackLayout::Shown {
            load: shown,
            stamp: TrackLayoutStamp::for_test(2),
        },
        ..same
    };
    assert_eq!(
        carry.settle(now, relayed),
        CarrySettle::Ended(CarryEnd::SwitchedInPlace)
    );
    assert_eq!(
        carry.on_new_generation(generation(2), now, AdOnScreen::No),
        NewGenerationCarry::NotCarried
    );
}

#[test]
fn a_page_reset_before_it_answered_our_seek_does_not_override_the_position() {
    let now = Instant::now();
    let mut carry = placed_carry(now);
    assert_eq!(
        carry.judge_page_seek(Duration::ZERO, exact),
        CarryPageSeek::AbsorbedBeforeOurSeek
    );
    assert_eq!(carry.target(), Some(TARGET));
}

#[test]
fn after_the_answer_a_page_return_is_repeated_once_and_then_the_page_wins() {
    let now = Instant::now();
    let mut carry = placed_carry(now);
    carry.note_player_seek_answered(TARGET, exact);
    assert_eq!(
        carry.judge_page_seek(Duration::from_secs(3), exact),
        CarryPageSeek::RepeatToPage(TARGET)
    );
    // Повтор ждёт ответа: самовольство до него снова поглощается.
    assert_eq!(
        carry.judge_page_seek(Duration::from_secs(3), exact),
        CarryPageSeek::AbsorbedBeforeOurSeek
    );
    carry.note_player_seek_answered(TARGET, exact);
    assert_eq!(
        carry.judge_page_seek(Duration::from_secs(3), exact),
        CarryPageSeek::PageWins,
        "no third seek: the page keeps its place"
    );
    assert_eq!(carry.target(), None);
}

#[test]
fn an_echo_of_the_carried_position_is_obedience() {
    let now = Instant::now();
    let mut carry = placed_carry(now);
    assert_eq!(
        carry.judge_page_seek(TARGET, exact),
        CarryPageSeek::EchoOfCarry
    );
    // Эхо = ответ: дальнейший уход страницы — уже повод для повтора, а не сброс.
    assert_eq!(
        carry.judge_page_seek(Duration::ZERO, exact),
        CarryPageSeek::RepeatToPage(TARGET)
    );
}

#[test]
fn the_shown_generation_ends_the_carry_after_the_confirmation_window() {
    let now = Instant::now();
    let mut carry = placed_carry(now);
    let shown = view(PlacedGenerationShown::Shown, AdOnScreen::No);
    assert_eq!(carry.settle(now, shown), CarrySettle::Nothing);
    assert_eq!(
        carry.settle(now + VIEWER_WILL_CONFIRMATION_WINDOW / 2, shown),
        CarrySettle::Nothing
    );
    assert_eq!(
        carry.settle(now + VIEWER_WILL_CONFIRMATION_WINDOW, shown),
        CarrySettle::Ended(CarryEnd::Applied)
    );
}

#[test]
fn an_ad_appearing_on_the_placed_generation_rolls_the_carry_back() {
    let now = Instant::now();
    let mut carry = placed_carry(now);
    assert_eq!(
        carry.settle(now, view(PlacedGenerationShown::Shown, AdOnScreen::Yes)),
        CarrySettle::RolledBackForAd(generation(2))
    );
    assert_eq!(carry.placed_generation(), None);
    assert_eq!(
        carry.on_new_generation(generation(3), now, AdOnScreen::No),
        NewGenerationCarry::PlaceAt(TARGET),
        "the film after the ad still gets the position"
    );
}

#[test]
fn a_generation_replacing_the_placed_one_keeps_the_carry_unless_it_is_an_ad() {
    let now = Instant::now();
    let mut carry = placed_carry(now);
    assert_eq!(
        carry.on_new_generation(generation(3), now, AdOnScreen::No),
        NewGenerationCarry::AlreadyPlaced
    );
    assert_eq!(carry.placed_generation(), Some(generation(3)));
    assert_eq!(
        carry.on_new_generation(generation(4), now, AdOnScreen::Yes),
        NewGenerationCarry::SkippedAd
    );
    assert_eq!(carry.placed_generation(), None);
    assert_eq!(carry.target(), Some(TARGET));
}

#[test]
fn forgetting_and_rearming_replace_the_old_selection() {
    let now = Instant::now();
    let mut carry = armed_carry(now, ShownTrackLayout::Unknown);
    carry.forget(CarryEnd::ViewerSeeked);
    assert_eq!(carry.target(), None);
    carry.arm(Duration::from_secs(7), now, ShownTrackLayout::Unknown);
    carry.arm(Duration::from_secs(9), now, ShownTrackLayout::Unknown);
    assert_eq!(carry.target(), Some(Duration::from_secs(9)));
}
