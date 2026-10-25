//! Проба каталога: вердикт по уровню и по настоящему контексту без окна.

use gles2_egui::{EglVersion, MissingRequirement};
use render_shell_api::RendererUnusableReason;

use super::*;

const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";

/// Уровень без нехваток — годен; нехватка называется по имени из спецификации.
#[test]
fn baseline_verdict_names_the_first_missing_requirement() {
    assert_eq!(
        probe_of_baseline(&TierStatus::Available),
        RendererProbe::Usable
    );

    let missing = TierStatus::Missing(vec![
        MissingRequirement::EglVersion {
            found: EglVersion { major: 1, minor: 4 },
        },
        MissingRequirement::GlExtension("GL_OES_EGL_image"),
    ]);
    match probe_of_baseline(&missing) {
        RendererProbe::Unusable {
            reason: RendererUnusableReason::MissingRequiredFeature(feature),
        } => assert_eq!(feature.specification_name(), "EGL 1.5"),
        other => panic!("expected the EGL 1.5 shortfall, got {other:?}"),
    }
}

/// Уровень «недоступен» без названной нехватки — отказ создания, а не `Usable`.
#[test]
fn an_unavailable_tier_without_a_reason_is_not_usable() {
    assert!(matches!(
        probe_of_baseline(&TierStatus::Missing(Vec::new())),
        RendererProbe::Unusable {
            reason: RendererUnusableReason::CreationFailed(_)
        }
    ));
}

/// Настоящая проба: где есть EGL с GLES2 (в том числе llvmpipe) — `Usable`; где нет — причина
/// вместо паники. `GLES2_EGUI_REQUIRE_EGL=1` превращает «нет EGL» в падение.
#[test]
fn real_probe_reports_usable_or_a_typed_reason() {
    match probe_gles2_renderer() {
        RendererProbe::Usable => {}
        RendererProbe::Unusable { reason } => {
            assert!(
                std::env::var_os(REQUIRE_EGL_VARIABLE).is_none(),
                "EGL is required but the probe says unusable: {reason:?}"
            );
            eprintln!("skip real_probe: no usable GLES2 here: {reason:?}");
        }
    }
}
