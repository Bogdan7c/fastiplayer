//! Настоящий egui-кадр: клик по кнопке качества, меню, пункт «Change quality on the page».

use std::collections::{BTreeMap, BTreeSet};

use egui::{Event, Modifiers, PointerButton, Pos2, pos2};

use super::super::layout::{choose_mode, layout};
use super::super::menu::{CHANGE_QUALITY_ITEM, EXTENSION_TOO_OLD_HINT};
use super::super::{QualityControlAction, QualityControlFrame};
use super::chain;
use crate::page_quality::PageViewOffer;
use crate::state::QualityMenuView;
use crate::ui::player_controls::ControlAction;
use crate::ui::test_frame::{app_behavior_context, input_at, run_ui_frame};

/// Окно, в котором кнопка качества помещается рядом с «♪» и CC.
const WINDOW: f32 = 1_000.0;
/// Подпись кнопки для кадра 1280×720.
const BUTTON_LABEL: &str = "Video quality 720p";

/// Сцена: контекст egui, время кадров и узлы AccessKit последнего кадра.
struct Scene {
    context: egui::Context,
    time_seconds: f64,
    node_centers: BTreeMap<String, Pos2>,
    disabled_nodes: BTreeSet<String>,
    /// Отмеченные радио-пункты (отметка «выбрано на странице»).
    checked_nodes: BTreeSet<String>,
}

impl Scene {
    fn new() -> Self {
        let context = app_behavior_context();
        context.enable_accesskit();
        Self {
            context,
            time_seconds: 0.0,
            node_centers: BTreeMap::new(),
            disabled_nodes: BTreeSet::new(),
            checked_nodes: BTreeSet::new(),
        }
    }

    /// Один кадр с событиями; возвращает действия кадра.
    fn frame(&mut self, events: Vec<Event>, view: &QualityMenuView) -> Vec<ControlAction> {
        self.time_seconds += 0.05;
        let neighbor = chain(WINDOW, true, true).quality;
        let mut actions = Vec::new();
        let output = run_ui_frame(&self.context, input_at(self.time_seconds, events), |ui| {
            let frame = QualityControlFrame::prepare(ui, neighbor, view, true);
            frame.render(ui, view, true, &mut actions);
        });
        if let Some(update) = output.platform_output.accesskit_update {
            self.node_centers.clear();
            self.disabled_nodes.clear();
            self.checked_nodes.clear();
            for (_, node) in &update.nodes {
                let Some(label) = node.label().or_else(|| node.value()) else {
                    continue;
                };
                if node.is_disabled() {
                    self.disabled_nodes.insert(label.to_owned());
                }
                if node.toggled() == Some(egui::accesskit::Toggled::True) {
                    self.checked_nodes.insert(label.to_owned());
                }
                if let Some(bounds) = node.bounds() {
                    self.node_centers.insert(
                        label.to_owned(),
                        pos2(
                            ((bounds.x0 + bounds.x1) / 2.0) as f32,
                            ((bounds.y0 + bounds.y1) / 2.0) as f32,
                        ),
                    );
                }
            }
        }
        actions
    }

    /// Несколько пустых кадров: попап проходит свой «измерительный» кадр.
    fn settle(&mut self, view: &QualityMenuView) {
        for _ in 0..3 {
            let _ = self.frame(Vec::new(), view);
        }
    }

    /// Наведение, нажатие и отпускание левой кнопки; возвращает действия отпускания.
    fn left_click(&mut self, position: Pos2, view: &QualityMenuView) -> Vec<ControlAction> {
        let button_event = |pressed| Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let hover = self.frame(vec![Event::PointerMoved(position)], view);
        let press = self.frame(vec![button_event(true)], view);
        assert!(hover.is_empty() && press.is_empty());
        self.frame(vec![button_event(false)], view)
    }

    fn center_of(&self, label: &str) -> Pos2 {
        *self.node_centers.get(label).unwrap_or_else(|| {
            panic!(
                "no AccessKit node `{label}` in {:?}",
                self.node_centers.keys()
            )
        })
    }

    fn has_node(&self, label: &str) -> bool {
        self.node_centers.contains_key(label)
    }

    /// Открывает меню кликом по кнопке.
    fn open_menu(&mut self, view: &QualityMenuView) {
        self.settle(view);
        let button = self.center_of(BUTTON_LABEL);
        assert!(
            self.left_click(button, view).is_empty(),
            "the button only opens the menu"
        );
        self.settle(view);
    }
}

fn view(offer: PageViewOffer) -> QualityMenuView {
    QualityMenuView::sample_for_tests((1280, 720), offer)
}

/// Главное: клик по кнопке открывает меню, клик по пункту отдаёт `ShowPage` и закрывает меню.
#[test]
fn clicking_the_item_asks_to_show_the_page_and_closes_the_menu() {
    let view = view(PageViewOffer::Available);
    let mut scene = Scene::new();
    scene.open_menu(&view);
    assert!(
        egui::Popup::is_any_open(&scene.context),
        "the button opens the menu"
    );

    let item = scene.center_of(CHANGE_QUALITY_ITEM);
    let actions = scene.left_click(item, &view);
    assert_eq!(
        actions,
        vec![ControlAction::Quality(QualityControlAction::ShowPage)]
    );
    scene.settle(&view);
    assert!(
        !egui::Popup::is_any_open(&scene.context),
        "choosing an item closes the menu"
    );
}

/// Меню показывает то, что играет (из снимка), а не заглушку.
#[test]
fn the_menu_shows_resolution_codec_and_frame_rate_from_the_snapshot() {
    let view = view(PageViewOffer::Available);
    let mut scene = Scene::new();
    scene.open_menu(&view);
    for expected in [
        "Resolution",
        "1280 × 720",
        "Bitrate",
        "—",
        "Codec",
        "VP9",
        "Frame rate",
        "60 fps",
    ] {
        assert!(
            scene.has_node(expected),
            "`{expected}` missing in {:?}",
            scene.node_centers.keys()
        );
    }
}

/// Экстеншен устарел: пункт виден, но выключен — клик не даёт действия.
#[test]
fn with_an_old_extension_the_item_is_disabled_and_gives_no_action() {
    let view = view(PageViewOffer::ExtensionTooOld);
    let mut scene = Scene::new();
    scene.open_menu(&view);
    assert!(
        scene.disabled_nodes.contains(CHANGE_QUALITY_ITEM),
        "the item is disabled: {:?}",
        scene.disabled_nodes
    );
    let item = scene.center_of(CHANGE_QUALITY_ITEM);
    assert!(scene.left_click(item, &view).is_empty());
    // Подсказка выключенного пункта (egui рисует её только после паузы наведения).
    assert_eq!(
        EXTENSION_TOO_OLD_HINT,
        "Update the Fastiplayer Bridge extension to use this"
    );
}

/// Страницы нет или она не подключена: пункта в меню нет вовсе.
#[test]
fn when_the_page_cannot_be_asked_there_is_no_item() {
    for offer in [PageViewOffer::PageAway, PageViewOffer::NotAttached] {
        let view = view(offer);
        let mut scene = Scene::new();
        scene.open_menu(&view);
        assert!(!scene.has_node(CHANGE_QUALITY_ITEM), "{offer:?}");
        assert!(
            scene.has_node("Resolution"),
            "rows are still shown for {offer:?}"
        );
    }
}

/// Нет кадра — нет кнопки: ни виджета, ни действия на клик по её месту.
#[test]
fn without_a_frame_there_is_no_button() {
    let hidden = QualityMenuView::hidden();
    let mut scene = Scene::new();
    scene.settle(&hidden);
    assert!(!scene.has_node(BUTTON_LABEL));
    let neighbor = chain(WINDOW, true, true).quality;
    let place = layout(&neighbor, choose_mode(&neighbor), 1.0)
        .button_rect
        .expect("there is room")
        .center();
    assert!(scene.left_click(place, &hidden).is_empty());
    assert!(!egui::Popup::is_any_open(&scene.context));
}

// ---- качества страницы (14a) ---------------------------------------------------------

/// Вид с качествами страницы «1080p» и «720p» (отмечено «720p»).
fn view_with_page_qualities(
    availability: crate::page_variants::PageVariantAvailability,
) -> QualityMenuView {
    use std::sync::Arc;

    use crate::page_variants::{
        PageVariantIndex, PageVariantKind, PageVariantMenuRevision, PageVariantMenuView,
    };
    let menu = PageVariantMenuView::new(
        PageVariantKind::Quality,
        PageVariantMenuRevision::new(9),
        vec![Arc::from("1080p"), Arc::from("720p")],
        Some(PageVariantIndex::new(1)),
    );
    view(PageViewOffer::Available).with_page_qualities(Some(&menu), availability)
}

#[test]
fn page_qualities_are_listed_with_the_mark_above_the_page_item_and_a_pick_is_published() {
    use crate::page_variants::{PageVariantAvailability, PageVariantIndex};
    let view = view_with_page_qualities(PageVariantAvailability::Selectable);
    let mut scene = Scene::new();
    scene.open_menu(&view);

    assert_eq!(
        scene.checked_nodes,
        BTreeSet::from(["720p".to_owned()]),
        "one mark, on the quality the page selected"
    );
    let page_item_y = scene.center_of(CHANGE_QUALITY_ITEM).y;
    for quality in ["1080p", "720p"] {
        assert!(
            scene.center_of(quality).y < page_item_y,
            "`{quality}` is above `{CHANGE_QUALITY_ITEM}`"
        );
    }
    // Подпись кнопки по-прежнему по картинке на экране (13c).
    assert!(scene.has_node(BUTTON_LABEL));

    let actions = scene.left_click(scene.center_of("1080p"), &view);
    let expected = view
        .page_qualities()
        .and_then(|choices| choices.menu().choice(PageVariantIndex::new(0)))
        .expect("1080p");
    assert_eq!(
        actions,
        vec![ControlAction::Quality(
            QualityControlAction::SelectPageQuality(expected)
        )]
    );
    scene.settle(&view);
    assert!(
        !egui::Popup::is_any_open(&scene.context),
        "a pick closes the menu"
    );
}

#[test]
fn during_an_ad_the_page_qualities_are_disabled_but_the_page_item_is_not() {
    use crate::page_variants::PageVariantAvailability;
    let view = view_with_page_qualities(PageVariantAvailability::BlockedDuringAd);
    let mut scene = Scene::new();
    scene.open_menu(&view);
    for quality in ["1080p", "720p"] {
        assert!(
            scene.disabled_nodes.contains(quality),
            "`{quality}`: {:?}",
            scene.disabled_nodes
        );
    }
    assert!(!scene.disabled_nodes.contains(CHANGE_QUALITY_ITEM));
    assert!(scene.left_click(scene.center_of("1080p"), &view).is_empty());
}
