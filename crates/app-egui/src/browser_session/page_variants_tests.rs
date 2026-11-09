//! Склад вариантов страницы: замена меню, чужой вид, субтитры, ревизии, подписи.

use browser_bridge_protocol::session::VariantOption;
use browser_bridge_protocol::text::VariantLabel;

use super::*;

/// Меню вида `kind` из пар «id, подпись»; `selected` — номер отмеченного пункта.
pub(crate) fn menu(
    kind: VariantKind,
    options: &[(&str, &str)],
    selected: Option<usize>,
) -> VariantMenu {
    VariantMenu {
        kind,
        options: options
            .iter()
            .enumerate()
            .map(|(position, (id, label))| VariantOption {
                id: VariantId::new(*id).expect("variant id"),
                label: VariantLabel::new(*label).expect("variant label"),
                selection: if selected == Some(position) {
                    VariantSelection::Selected
                } else {
                    VariantSelection::Available
                },
            })
            .collect(),
    }
}

fn choice(
    kind: PageVariantKind,
    revision: PageVariantMenuRevision,
    index: usize,
) -> PageVariantChoice {
    PageVariantChoice {
        kind,
        revision,
        index: PageVariantIndex::new(index),
    }
}

fn stored_revision(outcome: MenuStore) -> PageVariantMenuRevision {
    match outcome {
        MenuStore::Stored(revision) => revision,
        other => panic!("the menu was not stored: {other:?}"),
    }
}

#[test]
fn without_a_menu_there_is_nothing_to_show_or_to_send() {
    let shelf = PageVariantShelf::default();
    assert_eq!(shelf.menu_view(PageVariantKind::Audio), None);
    assert_eq!(shelf.menu_view(PageVariantKind::Quality), None);
    assert_eq!(
        shelf.lookup(choice(
            PageVariantKind::Audio,
            PageVariantMenuRevision::new(1),
            0
        )),
        Err(VariantLookupRefusal::NoMenu)
    );
    assert_eq!(shelf.next_episode(), NextEpisodeAvailability::Unavailable);
}

#[test]
fn a_new_menu_replaces_the_old_one_and_makes_old_choices_stale() {
    let mut shelf = PageVariantShelf::default();
    let first = stored_revision(shelf.store_menu(menu(
        VariantKind::Audio,
        &[("orig", "Original"), ("dub", "Dub")],
        Some(0),
    )));
    let found = shelf
        .lookup(choice(PageVariantKind::Audio, first, 1))
        .expect("the dub is on the shelf");
    assert_eq!(found.variant_id.as_str(), "dub");
    assert_eq!(found.kind, VariantKind::Audio);
    assert_eq!(&*found.label, "Dub");

    let second = stored_revision(shelf.store_menu(menu(
        VariantKind::Audio,
        &[("orig", "Original"), ("dub", "Dub")],
        Some(1),
    )));
    assert!(second > first, "a changed menu gets a newer revision");
    assert_eq!(
        shelf.lookup(choice(PageVariantKind::Audio, first, 1)),
        Err(VariantLookupRefusal::StaleMenu),
        "a choice made against the replaced menu is stale"
    );
    let view = shelf.menu_view(PageVariantKind::Audio).expect("menu");
    assert_eq!(view.selected(), Some(PageVariantIndex::new(1)));
    assert_eq!(view.revision(), second);
}

#[test]
fn the_same_menu_again_keeps_the_revision_so_a_choice_stays_valid() {
    let mut shelf = PageVariantShelf::default();
    let options = [("a", "720p"), ("b", "1080p")];
    let revision = stored_revision(shelf.store_menu(menu(VariantKind::Quality, &options, None)));
    assert_eq!(
        shelf.store_menu(menu(VariantKind::Quality, &options, None)),
        MenuStore::Unchanged
    );
    assert!(
        shelf
            .lookup(choice(PageVariantKind::Quality, revision, 1))
            .is_ok()
    );
}

#[test]
fn a_menu_of_one_kind_leaves_the_other_kind_untouched() {
    let mut shelf = PageVariantShelf::default();
    let audio = stored_revision(shelf.store_menu(menu(
        VariantKind::Audio,
        &[("orig", "Original")],
        Some(0),
    )));
    let audio_view = shelf.menu_view(PageVariantKind::Audio);
    stored_revision(shelf.store_menu(menu(VariantKind::Quality, &[("q", "480p")], None)));
    stored_revision(shelf.store_menu(menu(VariantKind::Quality, &[("q", "720p")], None)));
    assert_eq!(shelf.menu_view(PageVariantKind::Audio), audio_view);
    assert!(
        shelf
            .lookup(choice(PageVariantKind::Audio, audio, 0))
            .is_ok(),
        "the audio revision is not made stale by quality menus"
    );
}

#[test]
fn subtitle_menus_are_skipped_and_counted() {
    let mut shelf = PageVariantShelf::default();
    assert_eq!(
        shelf.store_menu(menu(VariantKind::Subtitles, &[("en", "English")], Some(0))),
        MenuStore::SkippedSubtitles
    );
    assert_eq!(shelf.subtitle_menus_skipped(), 1);
    assert_eq!(shelf.menu_view(PageVariantKind::Audio), None);
    assert_eq!(shelf.menu_view(PageVariantKind::Quality), None);
}

#[test]
fn an_empty_menu_removes_the_kind() {
    let mut shelf = PageVariantShelf::default();
    let revision =
        stored_revision(shelf.store_menu(menu(VariantKind::Audio, &[("orig", "Original")], None)));
    assert!(matches!(
        shelf.store_menu(menu(VariantKind::Audio, &[], None)),
        MenuStore::Removed(_)
    ));
    assert_eq!(shelf.menu_view(PageVariantKind::Audio), None);
    assert_eq!(
        shelf.lookup(choice(PageVariantKind::Audio, revision, 0)),
        Err(VariantLookupRefusal::NoMenu)
    );
    assert_eq!(
        shelf.store_menu(menu(VariantKind::Audio, &[], None)),
        MenuStore::Unchanged,
        "an empty menu without a menu changes nothing"
    );
}

#[test]
fn a_number_outside_the_menu_is_refused() {
    let mut shelf = PageVariantShelf::default();
    let revision =
        stored_revision(shelf.store_menu(menu(VariantKind::Quality, &[("q", "720p")], None)));
    assert_eq!(
        shelf.lookup(choice(PageVariantKind::Quality, revision, 1)),
        Err(VariantLookupRefusal::NoSuchOption)
    );
}

#[test]
fn a_new_session_empties_the_shelf_but_old_revisions_never_come_back() {
    let mut shelf = PageVariantShelf::default();
    let old = stored_revision(shelf.store_menu(menu(VariantKind::Audio, &[("a", "A")], None)));
    shelf.note_navigation(NextEpisodeAvailability::Available);
    shelf.start_new_session();
    assert_eq!(shelf.menu_view(PageVariantKind::Audio), None);
    assert_eq!(shelf.next_episode(), NextEpisodeAvailability::Unavailable);
    let new = stored_revision(shelf.store_menu(menu(VariantKind::Audio, &[("a", "A")], None)));
    assert_ne!(
        old, new,
        "a choice from the previous page never matches the new one"
    );
    assert_eq!(
        shelf.lookup(choice(PageVariantKind::Audio, old, 0)),
        Err(VariantLookupRefusal::StaleMenu)
    );
}

#[test]
fn the_last_navigation_word_wins() {
    let mut shelf = PageVariantShelf::default();
    assert!(shelf.note_navigation(NextEpisodeAvailability::Available));
    assert!(!shelf.note_navigation(NextEpisodeAvailability::Available));
    assert_eq!(shelf.next_episode(), NextEpisodeAvailability::Available);
    assert!(shelf.note_navigation(NextEpisodeAvailability::Unavailable));
    assert_eq!(shelf.next_episode(), NextEpisodeAvailability::Unavailable);
}

#[test]
fn labels_reach_the_window_clean_and_an_emptied_label_gets_a_fallback() {
    // Протокол не пустит управляющие символы; здесь — пробелы по краям, которые очистка
    // снимает, и подпись из одних пробелов (после очистки пустая → «Audio 2»).
    let mut shelf = PageVariantShelf::default();
    stored_revision(shelf.store_menu(menu(
        VariantKind::Audio,
        &[("a", "  Українська  "), ("b", "   ")],
        None,
    )));
    let view = shelf.menu_view(PageVariantKind::Audio).expect("menu");
    let labels: Vec<&str> = view.options().map(|option| option.label).collect();
    assert_eq!(labels, ["Українська", "Audio 2"]);
}
