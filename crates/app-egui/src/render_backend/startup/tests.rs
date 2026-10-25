//! Старт рендера целиком: выбор, метка, создание и фолбэк на fake-кандидатах.

use std::collections::HashMap;

use fastiplayer_config::RenderProfile;
use render_shell_api::{
    RendererCreateError, RendererCreateStage, RendererFamily, RendererProbe, RendererUnusableReason,
};

use super::{StartupCandidates, UnfinishedStartupTracker, start_renderer};
use crate::render_backend::selection::{RendererStartupMarker, SelectionReason};

const WGPU: RendererFamily = RendererFamily::WgpuVulkan;
const GLES2: RendererFamily = RendererFamily::Gles2;

/// Fake-каталог: заданные пробы и заданные исходы создания; «рендер» — само семейство.
struct FakeCandidates {
    probes: HashMap<RendererFamily, RendererProbe>,
    creation_failures: HashMap<RendererFamily, RendererCreateError>,
    probed: Vec<RendererFamily>,
    created: Vec<RendererFamily>,
}

impl FakeCandidates {
    fn new(wgpu_probe: RendererProbe, gles2_probe: RendererProbe) -> Self {
        Self {
            probes: HashMap::from([(WGPU, wgpu_probe), (GLES2, gles2_probe)]),
            creation_failures: HashMap::new(),
            probed: Vec::new(),
            created: Vec::new(),
        }
    }

    fn failing_creation(mut self, family: RendererFamily, message: &str) -> Self {
        self.creation_failures.insert(
            family,
            RendererCreateError::new(RendererCreateStage::DeviceCreation, message),
        );
        self
    }
}

impl StartupCandidates for FakeCandidates {
    type Renderer = RendererFamily;

    fn catalog_order(&self) -> Vec<RendererFamily> {
        vec![WGPU, GLES2]
    }

    fn probe(&mut self, family: RendererFamily) -> RendererProbe {
        self.probed.push(family);
        self.probes[&family].clone()
    }

    fn create(&mut self, family: RendererFamily) -> Result<RendererFamily, RendererCreateError> {
        self.created.push(family);
        match self.creation_failures.get(&family) {
            Some(error) => Err(error.clone()),
            None => Ok(family),
        }
    }
}

/// Событие метки в порядке возникновения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerEvent {
    Starting(RendererFamily),
    Failed(RendererFamily),
    Probing(RendererFamily),
}

struct RecordingTracker {
    marker: RendererStartupMarker,
    events: Vec<MarkerEvent>,
}

impl RecordingTracker {
    fn clean() -> Self {
        Self::with_marker(RendererStartupMarker::NoUnfinishedStartup)
    }

    fn with_marker(marker: RendererStartupMarker) -> Self {
        Self {
            marker,
            events: Vec::new(),
        }
    }
}

impl UnfinishedStartupTracker for RecordingTracker {
    fn unfinished_startup(&self) -> RendererStartupMarker {
        self.marker
    }

    fn probe_starting(&mut self, family: RendererFamily) {
        self.events.push(MarkerEvent::Probing(family));
    }

    fn creation_starting(&mut self, family: RendererFamily) {
        self.events.push(MarkerEvent::Starting(family));
    }

    fn creation_failed(&mut self, family: RendererFamily) {
        self.events.push(MarkerEvent::Failed(family));
    }
}

fn no_vulkan() -> RendererProbe {
    RendererProbe::Unusable {
        reason: RendererUnusableReason::NoHardwareAdapter,
    }
}

/// Обычный старт: создан первый кандидат, второй не пробовался и не создавался, метка
/// записана ровно один раз, уведомления нет.
#[test]
fn healthy_machine_starts_the_first_candidate_and_touches_nothing_else() {
    let mut candidates = FakeCandidates::new(RendererProbe::Usable, RendererProbe::Usable);
    let mut tracker = RecordingTracker::clean();

    let started = start_renderer(RenderProfile::Auto, &mut candidates, &mut tracker)
        .expect("healthy machine starts");

    assert_eq!(started.renderer, WGPU);
    assert_eq!(started.report.family(), WGPU);
    assert_eq!(started.report.reason(), &SelectionReason::AutoFirstChoice);
    assert_eq!(started.report.user_notice(), None);
    assert_eq!(candidates.probed, vec![WGPU]);
    assert_eq!(candidates.created, vec![WGPU]);
    assert_eq!(
        tracker.events,
        vec![MarkerEvent::Probing(WGPU), MarkerEvent::Starting(WGPU)]
    );
}

/// Машина без аппаратного Vulkan: GLES2 создаётся молча (уведомления нет, причина в отчёте).
#[test]
fn machine_without_hardware_vulkan_starts_gles2_silently() {
    let mut candidates = FakeCandidates::new(no_vulkan(), RendererProbe::Usable);
    let mut tracker = RecordingTracker::clean();

    let started = start_renderer(RenderProfile::Auto, &mut candidates, &mut tracker)
        .expect("gles2 is usable");

    assert_eq!(started.renderer, GLES2);
    assert_eq!(started.report.user_notice(), None);
    assert_eq!(
        started.report.telemetry_text(),
        "OpenGL ES 2.0 — Auto: no hardware Vulkan adapter"
    );
    assert_eq!(
        candidates.created,
        vec![GLES2],
        "unusable wgpu is never created"
    );
}

/// Ошибка создания первого кандидата в Auto: метка снимается, следующий создаётся, и
/// пользователь получает уведомление.
#[test]
fn creation_failure_in_auto_falls_back_with_a_notice() {
    let mut candidates = FakeCandidates::new(RendererProbe::Usable, RendererProbe::Usable)
        .failing_creation(WGPU, "device lost at startup");
    let mut tracker = RecordingTracker::clean();

    let started = start_renderer(RenderProfile::Auto, &mut candidates, &mut tracker)
        .expect("gles2 takes over");

    assert_eq!(started.renderer, GLES2);
    assert!(matches!(
        started.report.reason(),
        SelectionReason::CreationFailed { failed } if failed.len() == 1 && failed[0].family == WGPU
    ));
    let notice = started.report.user_notice().expect("failure is reported");
    assert!(notice.contains("The Vulkan renderer could not start"));
    assert_eq!(
        tracker.events,
        vec![
            MarkerEvent::Probing(WGPU),
            MarkerEvent::Starting(WGPU),
            MarkerEvent::Failed(WGPU),
            MarkerEvent::Probing(GLES2),
            MarkerEvent::Starting(GLES2)
        ]
    );
}

/// Явный профиль `vulkan`, но создание упало: этот запуск идёт на GLES2 (решение 39), текст
/// уведомления объясняет, что настройка не менялась.
#[test]
fn creation_failure_with_explicit_profile_still_starts_the_other_renderer() {
    let mut candidates = FakeCandidates::new(RendererProbe::Usable, RendererProbe::Usable)
        .failing_creation(WGPU, "no surface");
    let mut tracker = RecordingTracker::clean();

    let started = start_renderer(RenderProfile::Vulkan, &mut candidates, &mut tracker)
        .expect("fallback works for explicit profile");

    assert_eq!(started.renderer, GLES2);
    let notice = started.report.user_notice().expect("failure is reported");
    assert!(notice.contains("Settings still say Vulkan"));
    assert!(notice.contains("the setting is unchanged"));
}

/// Упали все: ошибка — последняя ошибка создания, у каждого семейства ровно одна попытка и
/// каждая проба выполнена один раз; метка снята после последнего сбоя.
#[test]
fn every_creation_failing_is_fatal_with_the_last_error() {
    let mut candidates = FakeCandidates::new(RendererProbe::Usable, RendererProbe::Usable)
        .failing_creation(WGPU, "vulkan failed")
        .failing_creation(GLES2, "egl failed");
    let mut tracker = RecordingTracker::clean();

    let error = match start_renderer(RenderProfile::Auto, &mut candidates, &mut tracker) {
        Ok(_) => panic!("must fail"),
        Err(error) => error,
    };

    assert_eq!(error.message, "egl failed");
    assert_eq!(candidates.created, vec![WGPU, GLES2]);
    assert_eq!(candidates.probed, vec![WGPU, GLES2]);
    assert_eq!(tracker.events.last(), Some(&MarkerEvent::Failed(GLES2)));
}

/// Ни один кандидат не годен по пробам: ничего не создаётся, метка не пишется, ошибка
/// называет причину по каждому.
#[test]
fn nothing_usable_fails_without_creating_anything_but_marks_every_probe() {
    let mut candidates = FakeCandidates::new(
        no_vulkan(),
        RendererProbe::Unusable {
            reason: RendererUnusableReason::SoftwareAdapterOnly,
        },
    );
    let mut tracker = RecordingTracker::clean();

    let error = match start_renderer(RenderProfile::Auto, &mut candidates, &mut tracker) {
        Ok(_) => panic!("must fail"),
        Err(error) => error,
    };

    assert_eq!(error.stage, RendererCreateStage::AdapterSelection);
    assert!(error.message.contains("no hardware Vulkan adapter"));
    assert!(
        error
            .message
            .contains("only a software OpenGL ES 2.0 adapter")
    );
    assert!(candidates.created.is_empty());
    assert_eq!(
        tracker.events,
        vec![MarkerEvent::Probing(WGPU), MarkerEvent::Probing(GLES2)],
        "every probed family is marked first; nothing is created"
    );
}

/// Метка прошлого упавшего старта доходит до выбора: упавший Vulkan обходится без пробы.
#[test]
fn unfinished_startup_marker_from_the_tracker_steers_the_selection() {
    let mut candidates = FakeCandidates::new(RendererProbe::Usable, RendererProbe::Usable);
    let mut tracker = RecordingTracker::with_marker(RendererStartupMarker::UnfinishedStartup(WGPU));

    let started = start_renderer(RenderProfile::Vulkan, &mut candidates, &mut tracker)
        .expect("gles2 takes this launch");

    assert_eq!(started.renderer, GLES2);
    assert_eq!(
        started.report.reason(),
        &SelectionReason::RequestedStartupUnfinished { requested: WGPU }
    );
    assert!(started.report.user_notice().is_some());
    assert_eq!(candidates.probed, vec![GLES2]);
    assert_eq!(
        tracker.events,
        vec![MarkerEvent::Probing(GLES2), MarkerEvent::Starting(GLES2)]
    );
}

/// Метка пишется РАНЬШЕ пробы: проба, уронившая драйвер, не должна остаться без следа.
/// Журнал общий для фабрики и трекера, поэтому порядок видно напрямую.
#[test]
fn the_marker_is_recorded_before_each_probe() {
    use std::cell::RefCell;
    use std::rc::Rc;

    type Journal = Rc<RefCell<Vec<String>>>;

    struct JournalCandidates(Journal);
    impl StartupCandidates for JournalCandidates {
        type Renderer = RendererFamily;
        fn catalog_order(&self) -> Vec<RendererFamily> {
            vec![WGPU, GLES2]
        }
        fn probe(&mut self, family: RendererFamily) -> RendererProbe {
            self.0
                .borrow_mut()
                .push(format!("probe {}", family.stable_id()));
            match family {
                WGPU => no_vulkan(),
                _ => RendererProbe::Usable,
            }
        }
        fn create(
            &mut self,
            family: RendererFamily,
        ) -> Result<RendererFamily, RendererCreateError> {
            self.0
                .borrow_mut()
                .push(format!("create {}", family.stable_id()));
            Ok(family)
        }
    }
    struct JournalTracker(Journal);
    impl UnfinishedStartupTracker for JournalTracker {
        fn unfinished_startup(&self) -> RendererStartupMarker {
            RendererStartupMarker::NoUnfinishedStartup
        }
        fn probe_starting(&mut self, family: RendererFamily) {
            self.0
                .borrow_mut()
                .push(format!("mark-probe {}", family.stable_id()));
        }
        fn creation_starting(&mut self, family: RendererFamily) {
            self.0
                .borrow_mut()
                .push(format!("mark-create {}", family.stable_id()));
        }
        fn creation_failed(&mut self, _family: RendererFamily) {}
    }

    let journal: Journal = Rc::default();
    let mut candidates = JournalCandidates(Rc::clone(&journal));
    let mut tracker = JournalTracker(Rc::clone(&journal));

    start_renderer(RenderProfile::Auto, &mut candidates, &mut tracker).expect("gles2 starts");

    let wgpu = WGPU.stable_id();
    let gles2 = GLES2.stable_id();
    assert_eq!(
        *journal.borrow(),
        vec![
            format!("mark-probe {wgpu}"),
            format!("probe {wgpu}"),
            format!("mark-probe {gles2}"),
            format!("probe {gles2}"),
            format!("mark-create {gles2}"),
            format!("create {gles2}"),
        ]
    );
}
