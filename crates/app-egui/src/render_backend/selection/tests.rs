//! Таблица исходов `select_renderer`: профиль × пробы кандидатов × метка прошлого старта.

use fastiplayer_config::RenderProfile;
use render_shell_api::{
    RendererCreateError, RendererCreateStage, RendererFamily, RendererProbe, RendererUnusableReason,
};

use super::{
    CandidateProbe, RendererSelection, RendererSelectionNotice, RendererStartupMarker,
    select_renderer,
};

const CLEAN_START: RendererStartupMarker = RendererStartupMarker::NoUnfinishedStartup;

fn usable(family: RendererFamily) -> CandidateProbe {
    CandidateProbe {
        family,
        probe: RendererProbe::Usable,
    }
}

fn unusable(family: RendererFamily, reason: RendererUnusableReason) -> CandidateProbe {
    CandidateProbe {
        family,
        probe: RendererProbe::Unusable { reason },
    }
}

fn create(family: RendererFamily, notice: Option<RendererSelectionNotice>) -> RendererSelection {
    RendererSelection::Create { family, notice }
}

/// Каталог Linux сессии 02: один годный wgpu/Vulkan.
fn linux_session_02_catalog() -> Vec<CandidateProbe> {
    vec![usable(RendererFamily::WgpuVulkan)]
}

/// Каталог сессии 09: wgpu первым, GLES2 вторым.
fn wgpu_then_gles2(wgpu: RendererProbe, gles2: RendererProbe) -> Vec<CandidateProbe> {
    vec![
        CandidateProbe {
            family: RendererFamily::WgpuVulkan,
            probe: wgpu,
        },
        CandidateProbe {
            family: RendererFamily::Gles2,
            probe: gles2,
        },
    ]
}

/// Поведение сессии 02 без изменений для пользователя: Auto и Vulkan → wgpu без уведомлений,
/// OpenGles → wgpu с уведомлением «GLES2 не собран» (решение владельца).
#[test]
fn session_02_catalog_keeps_previous_startup_behaviour() {
    let catalog = linux_session_02_catalog();

    assert_eq!(
        select_renderer(RenderProfile::Auto, &catalog, CLEAN_START),
        create(RendererFamily::WgpuVulkan, None)
    );
    assert_eq!(
        select_renderer(RenderProfile::Vulkan, &catalog, CLEAN_START),
        create(RendererFamily::WgpuVulkan, None)
    );
    assert_eq!(
        select_renderer(RenderProfile::OpenGles, &catalog, CLEAN_START),
        create(
            RendererFamily::WgpuVulkan,
            Some(RendererSelectionNotice::RequestedFamilyNotBuilt {
                requested: RendererFamily::Gles2
            })
        )
    );
}

/// Ручной выбор не подменяется молча: негодный запрошенный рендер — отказ старта.
#[test]
fn manual_profile_never_silently_switches_to_another_built_family() {
    let no_vulkan = wgpu_then_gles2(
        RendererProbe::Unusable {
            reason: RendererUnusableReason::NoHardwareAdapter,
        },
        RendererProbe::Usable,
    );

    assert_eq!(
        select_renderer(RenderProfile::Vulkan, &no_vulkan, CLEAN_START),
        RendererSelection::NoUsableRenderer {
            rejected: vec![unusable(
                RendererFamily::WgpuVulkan,
                RendererUnusableReason::NoHardwareAdapter
            )]
        }
    );
    assert_eq!(
        select_renderer(RenderProfile::OpenGles, &no_vulkan, CLEAN_START),
        create(RendererFamily::Gles2, None)
    );
}

/// Auto берёт первый годный кандидат и сообщает, кого пропустил и почему.
#[test]
fn auto_skips_unusable_candidates_with_a_notice() {
    let software_vulkan_only = wgpu_then_gles2(
        RendererProbe::Unusable {
            reason: RendererUnusableReason::SoftwareAdapterOnly,
        },
        RendererProbe::Usable,
    );

    assert_eq!(
        select_renderer(RenderProfile::Auto, &software_vulkan_only, CLEAN_START),
        create(
            RendererFamily::Gles2,
            Some(RendererSelectionNotice::PreferredCandidateSkipped {
                skipped: unusable(
                    RendererFamily::WgpuVulkan,
                    RendererUnusableReason::SoftwareAdapterOnly
                )
            })
        )
    );
}

/// Метка упавшего старта: Auto пропускает этот рендер, если есть другой годный.
#[test]
fn auto_skips_family_whose_previous_startup_crashed() {
    let both_usable = wgpu_then_gles2(RendererProbe::Usable, RendererProbe::Usable);
    let wgpu_crashed = RendererStartupMarker::UnfinishedStartup(RendererFamily::WgpuVulkan);

    assert_eq!(
        select_renderer(RenderProfile::Auto, &both_usable, wgpu_crashed),
        create(
            RendererFamily::Gles2,
            Some(RendererSelectionNotice::PreferredCandidateSkipped {
                skipped: unusable(
                    RendererFamily::WgpuVulkan,
                    RendererUnusableReason::PreviousStartupCrashed
                )
            })
        )
    );
    // Метка не про первый кандидат — первый и выбирается, без уведомления.
    let gles2_crashed = RendererStartupMarker::UnfinishedStartup(RendererFamily::Gles2);
    assert_eq!(
        select_renderer(RenderProfile::Auto, &both_usable, gles2_crashed),
        create(RendererFamily::WgpuVulkan, None)
    );
}

/// Годен только упавший рендер: Auto пробует его ещё раз и об этом сообщает.
#[test]
fn auto_retries_the_only_usable_family_after_a_crash() {
    let wgpu_crashed = RendererStartupMarker::UnfinishedStartup(RendererFamily::WgpuVulkan);

    assert_eq!(
        select_renderer(
            RenderProfile::Auto,
            &linux_session_02_catalog(),
            wgpu_crashed
        ),
        create(
            RendererFamily::WgpuVulkan,
            Some(RendererSelectionNotice::RetryingAfterUnfinishedStartup {
                family: RendererFamily::WgpuVulkan
            })
        )
    );
}

/// Ни одного годного кандидата: отказ старта со всеми причинами (метка не «оживляет» негодный).
#[test]
fn no_usable_candidate_reports_every_rejection() {
    let creation_failed = RendererUnusableReason::CreationFailed(RendererCreateError::new(
        RendererCreateStage::DeviceCreation,
        "driver refused",
    ));
    let nothing_usable = wgpu_then_gles2(
        RendererProbe::Unusable {
            reason: RendererUnusableReason::NoHardwareAdapter,
        },
        RendererProbe::Unusable {
            reason: creation_failed.clone(),
        },
    );
    let expected = RendererSelection::NoUsableRenderer {
        rejected: nothing_usable.clone(),
    };

    assert_eq!(
        select_renderer(RenderProfile::Auto, &nothing_usable, CLEAN_START),
        expected
    );
    assert_eq!(
        select_renderer(
            RenderProfile::Auto,
            &nothing_usable,
            RendererStartupMarker::UnfinishedStartup(RendererFamily::Gles2)
        ),
        expected
    );
    assert_eq!(
        select_renderer(RenderProfile::OpenGles, &nothing_usable, CLEAN_START),
        RendererSelection::NoUsableRenderer {
            rejected: vec![unusable(RendererFamily::Gles2, creation_failed)]
        }
    );
}
