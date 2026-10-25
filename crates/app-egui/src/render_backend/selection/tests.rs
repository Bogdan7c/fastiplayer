//! Таблица исходов `select_renderer`: профиль × пробы кандидатов × метка прошлого старта.

use fastiplayer_config::RenderProfile;
use render_shell_api::{
    RendererCreateError, RendererCreateStage, RendererFamily, RendererProbe, RendererUnusableReason,
};

use super::{
    CandidateProbe, RendererSelection, RendererStartupMarker, SelectionReason, select_renderer,
};

const WGPU: RendererFamily = RendererFamily::WgpuVulkan;
const GLES2: RendererFamily = RendererFamily::Gles2;
const CLEAN_START: RendererStartupMarker = RendererStartupMarker::NoUnfinishedStartup;
const BOTH: [RendererFamily; 2] = [WGPU, GLES2];

fn usable() -> RendererProbe {
    RendererProbe::Usable
}

fn unusable(reason: RendererUnusableReason) -> RendererProbe {
    RendererProbe::Unusable { reason }
}

fn candidate(family: RendererFamily, probe: RendererProbe) -> CandidateProbe {
    CandidateProbe { family, probe }
}

fn create(family: RendererFamily, reason: SelectionReason) -> RendererSelection {
    RendererSelection::Create { family, reason }
}

fn crashed(family: RendererFamily) -> RendererStartupMarker {
    RendererStartupMarker::UnfinishedStartup(family)
}

/// Запускает выбор над таблицей проб и возвращает ещё и список реально выполненных проб.
fn run(
    profile: RenderProfile,
    order: &[RendererFamily],
    probes: &[(RendererFamily, RendererProbe)],
    marker: RendererStartupMarker,
) -> (RendererSelection, Vec<RendererFamily>) {
    let mut probed = Vec::new();
    let selection = select_renderer(profile, order, marker, &mut |family| {
        probed.push(family);
        probes
            .iter()
            .find(|(known, _)| *known == family)
            .map(|(_, probe)| probe.clone())
            .unwrap_or_else(|| unusable(RendererUnusableReason::NoHardwareAdapter))
    });
    (selection, probed)
}

fn no_vulkan() -> RendererProbe {
    unusable(RendererUnusableReason::NoHardwareAdapter)
}

/// Auto: первый годный кандидат берётся БЕЗ пробы остальных (старт на хорошем Vulkan не
/// дорожает) и без уведомления.
#[test]
fn auto_takes_the_first_usable_candidate_without_probing_the_rest() {
    let (selection, probed) = run(
        RenderProfile::Auto,
        &BOTH,
        &[(WGPU, usable()), (GLES2, usable())],
        CLEAN_START,
    );

    assert_eq!(selection, create(WGPU, SelectionReason::AutoFirstChoice));
    assert_eq!(probed, vec![WGPU]);
}

/// Auto на машине без аппаратного Vulkan (в том числе только lavapipe) идёт на GLES2 молча:
/// причина видна в телеметрии, но уведомления нет (решение 40).
#[test]
fn auto_skips_unusable_candidates_silently() {
    for reason in [
        RendererUnusableReason::NoHardwareAdapter,
        RendererUnusableReason::SoftwareAdapterOnly,
    ] {
        let (selection, probed) = run(
            RenderProfile::Auto,
            &BOTH,
            &[(WGPU, unusable(reason.clone())), (GLES2, usable())],
            CLEAN_START,
        );

        let expected_reason = SelectionReason::AutoSkippedUnusable {
            skipped: vec![candidate(WGPU, unusable(reason))],
        };
        assert!(!expected_reason.needs_user_notice());
        assert_eq!(selection, create(GLES2, expected_reason));
        assert_eq!(probed, vec![WGPU, GLES2]);
    }
}

/// Метка упавшего старта: Auto обходит это семейство БЕЗ его пробы и сообщает об этом.
#[test]
fn auto_skips_family_whose_previous_startup_crashed_and_tells_the_user() {
    let (selection, probed) = run(
        RenderProfile::Auto,
        &BOTH,
        &[(WGPU, usable()), (GLES2, usable())],
        crashed(WGPU),
    );

    let reason = SelectionReason::PreviousStartupUnfinishedInAuto { crashed: WGPU };
    assert!(reason.needs_user_notice());
    assert_eq!(selection, create(GLES2, reason));
    assert_eq!(probed, vec![GLES2], "the crashed family is not even probed");

    // Метка не про первого кандидата — первый и выбирается, без уведомления.
    let (selection, probed) = run(
        RenderProfile::Auto,
        &BOTH,
        &[(WGPU, usable()), (GLES2, usable())],
        crashed(GLES2),
    );
    assert_eq!(selection, create(WGPU, SelectionReason::AutoFirstChoice));
    assert_eq!(probed, vec![WGPU]);
}

/// Годен только упавший рендер: Auto пробует его ещё раз и об этом сообщает.
#[test]
fn auto_retries_the_only_usable_family_after_a_crash() {
    let (selection, _) = run(
        RenderProfile::Auto,
        &[WGPU],
        &[(WGPU, usable())],
        crashed(WGPU),
    );
    assert_eq!(
        selection,
        create(WGPU, SelectionReason::RetryingAfterUnfinishedStartup)
    );

    // Упавший негоден по пробе — метка его не «оживляет»: GLES2 годен и берётся.
    let (selection, _) = run(
        RenderProfile::Auto,
        &BOTH,
        &[(WGPU, no_vulkan()), (GLES2, usable())],
        crashed(WGPU),
    );
    assert_eq!(
        selection,
        create(
            GLES2,
            SelectionReason::PreviousStartupUnfinishedInAuto { crashed: WGPU }
        )
    );
}

/// Явный профиль выполняется как написан, без проб лишних кандидатов.
#[test]
fn explicit_profile_is_honoured_when_usable() {
    let (selection, probed) = run(
        RenderProfile::Vulkan,
        &BOTH,
        &[(WGPU, usable()), (GLES2, usable())],
        CLEAN_START,
    );
    assert_eq!(selection, create(WGPU, SelectionReason::ProfileHonoured));
    assert_eq!(probed, vec![WGPU]);

    let (selection, probed) = run(
        RenderProfile::OpenGles,
        &BOTH,
        &[(WGPU, usable()), (GLES2, usable())],
        CLEAN_START,
    );
    assert_eq!(selection, create(GLES2, SelectionReason::ProfileHonoured));
    assert_eq!(probed, vec![GLES2]);
}

/// Решение 39: явный профиль невыполним — этот запуск на другом годном кандидате с
/// уведомлением (в обе стороны); фатально только если негодны все.
#[test]
fn unusable_explicit_profile_falls_to_the_next_usable_candidate() {
    let (selection, _) = run(
        RenderProfile::Vulkan,
        &BOTH,
        &[(WGPU, no_vulkan()), (GLES2, usable())],
        CLEAN_START,
    );
    let reason = SelectionReason::RequestedUnusable {
        requested: WGPU,
        reason: RendererUnusableReason::NoHardwareAdapter,
    };
    assert!(reason.needs_user_notice());
    assert_eq!(selection, create(GLES2, reason));

    let missing_egl = RendererUnusableReason::MissingRequiredFeature(
        render_shell_api::RequiredRendererFeature::new("EGL_KHR_surfaceless_context"),
    );
    let (selection, _) = run(
        RenderProfile::OpenGles,
        &BOTH,
        &[(WGPU, usable()), (GLES2, unusable(missing_egl.clone()))],
        CLEAN_START,
    );
    assert_eq!(
        selection,
        create(
            WGPU,
            SelectionReason::RequestedUnusable {
                requested: GLES2,
                reason: missing_egl
            }
        )
    );
}

/// Решение 35: явный `vulkan` + метка «Vulkan не завершил старт» — этот запуск на GLES2 с
/// уведомлением; если GLES2 тоже негоден, Vulkan пробуется снова.
#[test]
fn explicit_profile_with_unfinished_startup_runs_once_on_the_next_candidate() {
    let (selection, probed) = run(
        RenderProfile::Vulkan,
        &BOTH,
        &[(WGPU, usable()), (GLES2, usable())],
        crashed(WGPU),
    );
    let reason = SelectionReason::RequestedStartupUnfinished { requested: WGPU };
    assert!(reason.needs_user_notice());
    assert_eq!(selection, create(GLES2, reason));
    assert_eq!(probed, vec![GLES2]);

    let (selection, _) = run(
        RenderProfile::Vulkan,
        &BOTH,
        &[(WGPU, usable()), (GLES2, no_vulkan())],
        crashed(WGPU),
    );
    assert_eq!(
        selection,
        create(WGPU, SelectionReason::RetryingAfterUnfinishedStartup)
    );

    // Метка про другое семейство явный выбор не трогает.
    let (selection, _) = run(
        RenderProfile::Vulkan,
        &BOTH,
        &[(WGPU, usable()), (GLES2, usable())],
        crashed(GLES2),
    );
    assert_eq!(selection, create(WGPU, SelectionReason::ProfileHonoured));
}

/// Создание, упавшее в этом запуске, приходит как проба `CreationFailed`: Auto уходит на
/// следующего с уведомлением, явный профиль — тоже (решение 39).
#[test]
fn creation_failure_moves_to_the_next_candidate_with_a_notice() {
    let creation_failed = unusable(RendererUnusableReason::CreationFailed(
        RendererCreateError::new(RendererCreateStage::DeviceCreation, "driver refused"),
    ));

    let (selection, _) = run(
        RenderProfile::Auto,
        &BOTH,
        &[(WGPU, creation_failed.clone()), (GLES2, usable())],
        CLEAN_START,
    );
    let reason = SelectionReason::CreationFailed {
        failed: vec![candidate(WGPU, creation_failed.clone())],
    };
    assert!(reason.needs_user_notice());
    assert_eq!(selection, create(GLES2, reason));

    let (selection, _) = run(
        RenderProfile::Vulkan,
        &BOTH,
        &[(WGPU, creation_failed), (GLES2, usable())],
        CLEAN_START,
    );
    assert!(matches!(
        selection,
        RendererSelection::Create {
            family: RendererFamily::Gles2,
            reason: SelectionReason::RequestedUnusable { .. }
        }
    ));
}

/// Семейства нет в каталоге сборки: берётся первый годный кандидат с уведомлением.
#[test]
fn requested_family_missing_from_the_catalog_uses_the_available_renderer() {
    let (selection, _) = run(
        RenderProfile::OpenGles,
        &[WGPU],
        &[(WGPU, usable())],
        CLEAN_START,
    );
    let reason = SelectionReason::RequestedNotBuilt { requested: GLES2 };
    assert!(reason.needs_user_notice());
    assert_eq!(selection, create(WGPU, reason));

    // Каталог из одного кандидата: Auto и Vulkan — как до GLES2, без уведомлений.
    for profile in [RenderProfile::Auto, RenderProfile::Vulkan] {
        let (selection, _) = run(profile, &[WGPU], &[(WGPU, usable())], CLEAN_START);
        assert!(matches!(
            selection,
            RendererSelection::Create {
                family: RendererFamily::WgpuVulkan,
                ref reason
            } if !reason.needs_user_notice()
        ));
    }
}

/// Ни одного годного кандидата: отказ старта со всеми причинами в порядке каталога, для
/// каждого профиля; метка не «оживляет» негодный.
#[test]
fn no_usable_candidate_reports_every_rejection() {
    let creation_failed = RendererUnusableReason::CreationFailed(RendererCreateError::new(
        RendererCreateStage::DeviceCreation,
        "driver refused",
    ));
    let probes = [
        (WGPU, no_vulkan()),
        (GLES2, unusable(creation_failed.clone())),
    ];
    let expected = RendererSelection::NoUsableRenderer {
        rejected: vec![
            candidate(WGPU, no_vulkan()),
            candidate(GLES2, unusable(creation_failed)),
        ],
    };

    for profile in [
        RenderProfile::Auto,
        RenderProfile::Vulkan,
        RenderProfile::OpenGles,
    ] {
        for marker in [CLEAN_START, crashed(GLES2), crashed(WGPU)] {
            let (selection, _) = run(profile, &BOTH, &probes, marker);
            assert_eq!(selection, expected, "{profile:?} with {marker:?}");
        }
    }
}

/// Полный перебор инвариантов: профиль × метка × (3 исхода пробы)². Выбранное семейство
/// всегда годно по пробе; «ничего не годно» — только если годных нет; каждое семейство
/// пробуется не более одного раза; уведомление не нужно, только когда сбоя не было.
#[test]
fn every_combination_keeps_the_selection_invariants() {
    let probe_outcomes = [
        usable(),
        no_vulkan(),
        unusable(RendererUnusableReason::SoftwareAdapterOnly),
    ];
    let markers = [CLEAN_START, crashed(WGPU), crashed(GLES2)];
    for profile in [
        RenderProfile::Auto,
        RenderProfile::Vulkan,
        RenderProfile::OpenGles,
    ] {
        for marker in markers {
            for wgpu_probe in &probe_outcomes {
                for gles2_probe in &probe_outcomes {
                    let probes = [(WGPU, wgpu_probe.clone()), (GLES2, gles2_probe.clone())];
                    let (selection, probed) = run(profile, &BOTH, &probes, marker);
                    let context = format!("{profile:?} {marker:?} {probes:?}");

                    for family in BOTH {
                        let probe_count = probed.iter().filter(|probed| **probed == family).count();
                        assert!(probe_count <= 1, "{family:?} probed twice: {context}");
                    }
                    match selection {
                        RendererSelection::Create { family, .. } => {
                            let family_probe =
                                &probes.iter().find(|(f, _)| *f == family).unwrap().1;
                            assert_eq!(*family_probe, usable(), "{context}");
                        }
                        RendererSelection::NoUsableRenderer { rejected } => {
                            assert!(
                                probes.iter().all(|(_, probe)| *probe != usable()),
                                "{context}"
                            );
                            assert_eq!(rejected.len(), 2, "{context}");
                        }
                    }
                }
            }
        }
    }
}
