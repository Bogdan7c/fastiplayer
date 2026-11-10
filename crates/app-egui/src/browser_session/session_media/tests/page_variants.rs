//! Варианты страницы в медиа-части (14a): склад меню, выбор зрителя → `select_variant`,
//! «следующая серия» → `next_episode`, и возврат на секунду после смены варианта
//! (решение 280) сквозь настоящий поток нового поколения.
//!
//! Плеер и страница подделаны: видна каждая команда плееру и каждое сообщение странице.
//! Позиция зрителя — 5 с у показанного поколения 1 (подделка плеера позволяет любую), а
//! поколение 2 — длинный ролик с данными только в начале (0–0,4 с): у T данных нет, и
//! странице нужна просьба перемотаться.

use browser_bridge_protocol::ProtocolVersion;
use browser_bridge_protocol::session::{
    AdPod, AdSkip, NextEpisodeAvailability, PageAdState, VariantKind,
};
use browser_bridge_protocol::text::VariantId;

use super::generation_placement::{
    HOLD_CHECK, LONG_CLIP, append_media_at, generation_with_head, page_seek_targets, start_of_load,
};
use super::*;
use crate::browser_session::page_variants::PageVariantUpdate;
use crate::browser_session::session_media::page_variants::{
    NEXT_EPISODE_REQUESTED_LINE, NextEpisodeRequestOutcome, PAGE_VARIANT_SELECTED_LINE,
    PageVariantRequestOutcome,
};
use crate::browser_session::session_media::position_carry::PositionCarryPolicy;
use crate::browser_session::test_log_capture::with_captured_log;
use crate::page_variants::{
    NextEpisodeOffer, PageVariantChoice, PageVariantIndex, PageVariantKind, PageVariantNotice,
};
use crate::viewer_transport::ViewerTransportIntent;

/// Секунда зрителя в миг выбора.
const VIEWER_SECOND: Duration = Duration::from_secs(5);

/// Поколение 1 показано на `VIEWER_SECOND`, общая версия `version`.
fn watching_harness(version: ProtocolVersion) -> (Harness, SessionRef, PushedStreamWriter) {
    let (mut harness, current, writer) = loaded_harness();
    harness.media.on_session_attached(&current, version);
    harness.player.show_load(
        harness.player.load_ticket(0),
        VIEWER_SECOND,
        PlaybackState::Playing,
    );
    harness.media.drive(Instant::now());
    harness.page.take_sent();
    harness.player.take_calls();
    (harness, current, writer)
}

fn menu_notice(session: &SessionRef, kind: VariantKind, ids: &[&str]) -> IntakeNotice {
    let options = ids
        .iter()
        .enumerate()
        .map(|(position, id)| (*id, *id, position == 0))
        .collect::<Vec<_>>();
    IntakeNotice::PageVariants {
        session: session.clone(),
        update: PageVariantUpdate::Menu(browser_bridge_protocol::session::VariantMenu {
            kind,
            options: options
                .into_iter()
                .map(
                    |(id, label, selected)| browser_bridge_protocol::session::VariantOption {
                        id: VariantId::new(id).expect("id"),
                        label: browser_bridge_protocol::text::VariantLabel::new(label)
                            .expect("label"),
                        selection: if selected {
                            browser_bridge_protocol::session::VariantSelection::Selected
                        } else {
                            browser_bridge_protocol::session::VariantSelection::Available
                        },
                    },
                )
                .collect(),
        }),
    }
}

fn navigation_notice(session: &SessionRef, availability: NextEpisodeAvailability) -> IntakeNotice {
    IntakeNotice::PageVariants {
        session: session.clone(),
        update: PageVariantUpdate::Navigation(availability),
    }
}

fn ad_notice(session: &SessionRef, state: PageAdState) -> IntakeNotice {
    IntakeNotice::AdState {
        session: session.clone(),
        state,
    }
}

fn ad_showing() -> PageAdState {
    PageAdState::Showing {
        pod: AdPod::Unknown {},
        skip: AdSkip::Unavailable {},
    }
}

/// Выбор пункта `index` по меню, которое сейчас видит окно.
fn current_choice(harness: &Harness, kind: PageVariantKind, index: usize) -> PageVariantChoice {
    harness
        .media
        .page_variant_facts()
        .menu(kind)
        .and_then(|menu| menu.choice(PageVariantIndex::new(index)))
        .expect("the menu offers this option")
}

/// Сообщения странице без часов и буфера.
fn commands_to_page(harness: &mut Harness) -> Vec<PlayerSessionMessage> {
    harness
        .page
        .take_sent()
        .into_iter()
        .filter(|message| {
            !matches!(
                message,
                PlayerSessionMessage::Clock { .. } | PlayerSessionMessage::BufferState { .. }
            )
        })
        .collect()
}

fn select(kind: VariantKind, id: &str) -> PlayerSessionMessage {
    PlayerSessionMessage::SelectVariant {
        kind,
        variant_id: VariantId::new(id).expect("id"),
    }
}

/// Зритель выбирает дубляж «dub» в меню озвучек; очередь уходит странице.
fn viewer_selects_dub(harness: &mut Harness) {
    let choice = current_choice(harness, PageVariantKind::Audio, 1);
    harness.media.on_viewer_transport(
        ViewerTransportIntent::SelectPageVariant(choice),
        Instant::now(),
    );
    harness.media.drive(Instant::now());
}

// ---- склад и выбор -----------------------------------------------------------------

#[test]
fn a_viewer_selection_reaches_the_page_and_the_window_is_told_once() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_7);
    harness.media.on_intake(
        vec![menu_notice(&current, VariantKind::Audio, &["orig", "dub"])],
        None,
    );
    let facts = harness.media.page_variant_facts();
    let audio = facts.menu(PageVariantKind::Audio).expect("audio menu");
    assert_eq!(audio.option_count(), 2);
    assert_eq!(audio.selected(), Some(PageVariantIndex::new(0)));
    assert_eq!(
        facts.menu(PageVariantKind::Quality),
        None,
        "the other kind is untouched"
    );

    let ((), logs) = with_captured_log(|| viewer_selects_dub(&mut harness));
    assert_eq!(
        commands_to_page(&mut harness),
        vec![select(VariantKind::Audio, "dub")]
    );
    assert!(
        logs.contains(&format!("{PAGE_VARIANT_SELECTED_LINE} kind=audio index=1")),
        "{logs}"
    );
    assert!(
        !logs.contains("\"dub\""),
        "the label stays out of the journal: {logs}"
    );
    assert_eq!(
        harness.media.take_page_variant_notice(),
        Some(PageVariantNotice::Selected {
            kind: PageVariantKind::Audio,
            label: "dub".into(),
        })
    );
    assert_eq!(harness.media.take_page_variant_notice(), None);
}

#[test]
fn without_a_menu_or_with_a_stale_choice_nothing_reaches_the_page() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_7);
    let stale = {
        harness.media.on_intake(
            vec![menu_notice(
                &current,
                VariantKind::Quality,
                &["720", "1080"],
            )],
            None,
        );
        current_choice(&harness, PageVariantKind::Quality, 1)
    };
    // Меню сменилось (страница сменила список) после того, как зритель его видел.
    harness.media.on_intake(
        vec![menu_notice(
            &current,
            VariantKind::Quality,
            &["480", "720", "1080"],
        )],
        None,
    );
    assert_eq!(
        harness
            .media
            .viewer_selects_page_variant(stale, Instant::now()),
        PageVariantRequestOutcome::StaleMenu
    );
    let no_menu = PageVariantChoice {
        kind: PageVariantKind::Audio,
        ..stale
    };
    assert_eq!(
        harness
            .media
            .viewer_selects_page_variant(no_menu, Instant::now()),
        PageVariantRequestOutcome::NoMenu
    );
    harness.media.drive(Instant::now());
    assert_eq!(commands_to_page(&mut harness), vec![]);
    assert_eq!(harness.media.take_page_variant_notice(), None);
}

#[test]
fn during_an_ad_the_selection_is_refused_like_a_seek() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_7);
    harness.media.on_intake(
        vec![
            menu_notice(&current, VariantKind::Audio, &["orig", "dub"]),
            ad_notice(&current, ad_showing()),
        ],
        None,
    );
    let choice = current_choice(&harness, PageVariantKind::Audio, 1);
    assert_eq!(
        harness
            .media
            .viewer_selects_page_variant(choice, Instant::now()),
        PageVariantRequestOutcome::RefusedDuringAd
    );
    harness.media.drive(Instant::now());
    assert_eq!(commands_to_page(&mut harness), vec![]);
}

/// Зритель выбирает строку `index` меню субтитров страницы.
fn viewer_selects_subtitle_line(harness: &mut Harness, index: usize) {
    let choice = current_choice(harness, PageVariantKind::Subtitles, index);
    harness.media.on_viewer_transport(
        ViewerTransportIntent::SelectPageVariant(choice),
        Instant::now(),
    );
    harness.media.drive(Instant::now());
}

#[test]
fn a_subtitle_menu_reaches_the_window_beside_the_other_menus() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_7);
    harness.media.on_intake(
        vec![menu_notice(&current, VariantKind::Subtitles, &["en", "de"])],
        None,
    );
    let facts = harness.media.page_variant_facts();
    let subtitles = facts
        .menu(PageVariantKind::Subtitles)
        .expect("the subtitle menu is kept");
    assert_eq!(subtitles.option_count(), 2);
    assert_eq!(subtitles.selected(), Some(PageVariantIndex::new(0)));
    assert_eq!(facts.menu(PageVariantKind::Audio), None);
    assert_eq!(facts.menu(PageVariantKind::Quality), None);
}

#[test]
fn a_subtitle_line_pick_sends_exactly_one_select_variant_and_arms_no_carry() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_7);
    harness.media.on_intake(
        vec![menu_notice(&current, VariantKind::Subtitles, &["en", "de"])],
        None,
    );

    let ((), logs) = with_captured_log(|| viewer_selects_subtitle_line(&mut harness, 1));

    assert_eq!(
        commands_to_page(&mut harness),
        vec![select(VariantKind::Subtitles, "de")],
        "exactly one select_variant for the subtitle line"
    );
    assert!(
        logs.contains(&format!(
            "{PAGE_VARIANT_SELECTED_LINE} kind=subtitles index=1"
        )),
        "{logs}"
    );
    assert_eq!(
        harness.media.take_page_variant_notice(),
        None,
        "no toast about a variant change"
    );
    // Поколения никто не менял: ни одна последующая загрузка не должна ехать к секунде зрителя.
    assert_no_carry_after(&mut harness, &current);
}

#[test]
fn a_subtitle_line_pick_during_an_ad_is_refused_and_a_stale_one_sends_nothing() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_7);
    harness.media.on_intake(
        vec![
            menu_notice(&current, VariantKind::Subtitles, &["en", "de"]),
            ad_notice(&current, ad_showing()),
        ],
        None,
    );
    let during_ad = current_choice(&harness, PageVariantKind::Subtitles, 1);
    assert_eq!(
        harness
            .media
            .viewer_selects_page_variant(during_ad, Instant::now()),
        PageVariantRequestOutcome::RefusedDuringAd
    );

    // Реклама ушла, меню сменилось: выбор по старому меню устарел.
    harness.media.on_intake(
        vec![
            ad_notice(&current, PageAdState::None {}),
            menu_notice(&current, VariantKind::Subtitles, &["en", "de", "fr"]),
        ],
        None,
    );
    assert_eq!(
        harness
            .media
            .viewer_selects_page_variant(during_ad, Instant::now()),
        PageVariantRequestOutcome::StaleMenu
    );
    harness.media.drive(Instant::now());
    assert_eq!(commands_to_page(&mut harness), vec![]);
}

// ---- следующая серия -----------------------------------------------------------------

#[test]
fn next_episode_goes_to_the_page_only_after_the_page_says_available() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_7);
    assert_eq!(
        harness.media.page_variant_facts().next_episode,
        NextEpisodeOffer::PageSaysUnavailable
    );
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::NextEpisode, Instant::now());
    harness.media.drive(Instant::now());
    assert_eq!(commands_to_page(&mut harness), vec![]);

    harness.media.on_intake(
        vec![navigation_notice(
            &current,
            NextEpisodeAvailability::Available,
        )],
        None,
    );
    assert_eq!(
        harness.media.page_variant_facts().next_episode,
        NextEpisodeOffer::Offered
    );
    let ((), logs) = with_captured_log(|| {
        harness
            .media
            .on_viewer_transport(ViewerTransportIntent::NextEpisode, Instant::now());
        harness.media.drive(Instant::now());
    });
    assert_eq!(
        commands_to_page(&mut harness),
        vec![PlayerSessionMessage::NextEpisode {}]
    );
    assert!(logs.contains(NEXT_EPISODE_REQUESTED_LINE), "{logs}");
    assert_eq!(
        harness.media.take_page_variant_notice(),
        Some(PageVariantNotice::NextEpisodeRequested)
    );
}

#[test]
fn with_a_1_6_extension_there_is_no_next_episode() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_6);
    harness.media.on_intake(
        vec![navigation_notice(
            &current,
            NextEpisodeAvailability::Available,
        )],
        None,
    );
    assert_eq!(
        harness.media.page_variant_facts().next_episode,
        NextEpisodeOffer::ExtensionTooOld
    );
    assert_eq!(
        harness.media.viewer_asks_next_episode(Instant::now()),
        NextEpisodeRequestOutcome::NotOffered(NextEpisodeOffer::ExtensionTooOld)
    );
    harness.media.drive(Instant::now());
    assert_eq!(commands_to_page(&mut harness), vec![]);
}

#[test]
fn during_an_ad_next_does_not_skip_the_episode() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_7);
    harness.media.on_intake(
        vec![
            navigation_notice(&current, NextEpisodeAvailability::Available),
            ad_notice(&current, ad_showing()),
        ],
        None,
    );
    assert_eq!(
        harness.media.viewer_asks_next_episode(Instant::now()),
        NextEpisodeRequestOutcome::RefusedDuringAd
    );
    harness.media.drive(Instant::now());
    assert_eq!(commands_to_page(&mut harness), vec![]);
}

// ---- возврат на секунду (решение 280) ------------------------------------------------

/// Поколение 2 (данные только в начале длинного ролика) открывает страница.
fn page_opens_generation_2(harness: &mut Harness, current: &SessionRef) -> PushedStreamWriter {
    let (writer, notice) = generation_with_head(current, 2, PushedDuration::Finite(LONG_CLIP));
    harness.media.on_intake(vec![notice], None);
    writer
}

#[test]
fn after_a_dub_switch_the_new_generation_is_placed_at_the_viewer_second() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_7);
    harness.media.on_intake(
        vec![menu_notice(&current, VariantKind::Audio, &["orig", "dub"])],
        None,
    );
    viewer_selects_dub(&mut harness);
    commands_to_page(&mut harness);

    let mut second = page_opens_generation_2(&mut harness, &current);
    harness.media.drive(Instant::now());
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        vec![VIEWER_SECOND],
        "one page seek to the viewer second"
    );
    // Новый MediaSource страницы стартует с нуля и говорит об этом раньше, чем исполнит
    // нашу просьбу: это не перебивает секунду зрителя (инвариант 4).
    harness.media.on_intake(
        vec![intent(
            &current,
            PageIntent::FollowPageSeek(MediaTime::from_micros(0).expect("zero")),
        )],
        None,
    );
    let held = harness.drive_for(HOLD_CHECK);
    assert!(
        !held.iter().any(|call| matches!(call, PlayerCall::Load(..))),
        "the generation waits for data at the viewer second: {held:?}"
    );
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        Vec::<Duration>::new()
    );

    append_media_at(&mut second, VIEWER_SECOND);
    harness.drive_until_loaded();
    assert_eq!(start_of_load(&harness, 1), Some(VIEWER_SECOND));
}

#[test]
fn an_ad_generation_in_between_starts_by_itself_and_the_film_gets_the_second() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_7);
    harness.media.on_intake(
        vec![menu_notice(&current, VariantKind::Audio, &["orig", "dub"])],
        None,
    );
    viewer_selects_dub(&mut harness);
    commands_to_page(&mut harness);

    // Ashdi: после смены озвучки — реклама отдельным роликом.
    harness
        .media
        .on_intake(vec![ad_notice(&current, ad_showing())], None);
    let (_ad_writer, ad_notice_generation) = stream_generation(&current, 2, true);
    harness.media.on_intake(vec![ad_notice_generation], None);
    harness.drive_until_loaded();
    assert_eq!(
        start_of_load(&harness, 1),
        None,
        "the ad starts at its own start"
    );
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        Vec::<Duration>::new()
    );

    // Реклама кончилась, страница открывает фильм.
    harness
        .media
        .on_intake(vec![ad_notice(&current, PageAdState::None {})], None);
    let (mut film, film_notice) =
        generation_with_head(&current, 3, PushedDuration::Finite(LONG_CLIP));
    harness.media.on_intake(vec![film_notice], None);
    harness.media.drive(Instant::now());
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        vec![VIEWER_SECOND]
    );
    append_media_at(&mut film, VIEWER_SECOND);
    harness.drive_until_loaded();
    assert_eq!(start_of_load(&harness, 2), Some(VIEWER_SECOND));
}

/// Выбор, затем `forget` одним из путей, затем поколение 2: никакой перемотки к T.
fn assert_no_carry_after(harness: &mut Harness, current: &SessionRef) {
    commands_to_page(harness);
    let _second = page_opens_generation_2(harness, current);
    harness.drive_for(Duration::from_millis(50));
    assert!(
        !page_seek_targets(&harness.page.take_sent()).contains(&VIEWER_SECOND),
        "the generation after the cleared carry is not moved to the viewer second"
    );
}

#[test]
fn a_viewer_seek_after_the_selection_clears_the_carry() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_7);
    harness.media.on_intake(
        vec![menu_notice(&current, VariantKind::Audio, &["orig", "dub"])],
        None,
    );
    viewer_selects_dub(&mut harness);
    harness.media.on_viewer_transport(
        ViewerTransportIntent::Seek(Duration::from_millis(200)),
        Instant::now(),
    );
    assert_no_carry_after(&mut harness, &current);
}

#[test]
fn next_episode_does_not_carry_the_position() {
    let (mut harness, current, _writer) = watching_harness(ProtocolVersion::V1_7);
    harness.media.on_intake(
        vec![
            menu_notice(&current, VariantKind::Audio, &["orig", "dub"]),
            navigation_notice(&current, NextEpisodeAvailability::Available),
        ],
        None,
    );
    viewer_selects_dub(&mut harness);
    assert_eq!(
        harness.media.viewer_asks_next_episode(Instant::now()),
        NextEpisodeRequestOutcome::Queued
    );
    assert_no_carry_after(&mut harness, &current);
}

#[test]
fn an_expired_carry_does_not_move_a_late_generation() {
    let (harness, current, writer) = watching_harness(ProtocolVersion::V1_7);
    let Harness {
        media,
        player,
        page,
        closer,
    } = harness;
    let mut harness = Harness {
        media: media
            .with_position_carry_policy(PositionCarryPolicy::new(Duration::from_millis(30))),
        player,
        page,
        closer,
    };
    let _writer = writer;
    harness.media.on_intake(
        vec![menu_notice(&current, VariantKind::Audio, &["orig", "dub"])],
        None,
    );
    viewer_selects_dub(&mut harness);
    std::thread::sleep(Duration::from_millis(60));
    assert_no_carry_after(&mut harness, &current);
}
