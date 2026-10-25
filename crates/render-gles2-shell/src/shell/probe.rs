//! Лёгкая проба для каталога рендеров: годится ли GLES2 на этой машине (без окна).
//!
//! Проба создаёт EGL-контекст без окна (surfaceless) и проверяет базовый уровень README плана:
//! OpenGL ES 2.0 и EGL 1.5. Программный GL (llvmpipe) годен: GLES2 — последний кандидат каталога,
//! и лучше играть медленно, чем не стартовать (решение владельца плана). Нехватка возможности
//! называется её именем из спецификации, остальные отказы — стадией создания.

use gles2_egui::{Gles2Context, SurfaceSize, TierStatus};
use render_shell_api::{RendererProbe, RendererUnusableReason};

use super::create_error::{pipeline_creation_error, required_feature_for, unusable_reason_for};

/// Сторона цели пробы: контексту нужна хоть какая-то цель, содержимое не рисуется.
const PROBE_TARGET_SIDE: u32 = 16;

/// Проба GLES2-рендера для каталога (не создаёт окно и не держит ресурсов после возврата).
#[must_use]
pub fn probe_gles2_renderer() -> RendererProbe {
    let Some(target) = SurfaceSize::new(PROBE_TARGET_SIDE, PROBE_TARGET_SIDE) else {
        return failed_probe("probe target", &"the probe target size is zero");
    };
    match Gles2Context::offscreen(target) {
        Ok(context) => probe_of_baseline(&context.capabilities().tiers().baseline),
        Err(error) => RendererProbe::Unusable {
            reason: unusable_reason_for(&error),
        },
    }
}

/// Проба не смогла выполнить проверку: причина — отказ создания на стадии сборки.
fn failed_probe(what: &str, detail: &dyn std::fmt::Display) -> RendererProbe {
    RendererProbe::Unusable {
        reason: RendererUnusableReason::CreationFailed(pipeline_creation_error(what, detail)),
    }
}

/// Базовый уровень контекста → вердикт пробы (первая нехватка называется в причине).
fn probe_of_baseline(baseline: &TierStatus) -> RendererProbe {
    match baseline {
        TierStatus::Available => RendererProbe::Usable,
        TierStatus::Missing(missing) => match missing.first() {
            Some(requirement) => RendererProbe::Unusable {
                reason: RendererUnusableReason::MissingRequiredFeature(required_feature_for(
                    requirement,
                )),
            },
            // Уровень «недоступен», но нехватки не назвал: драйвер сломан — сообщаем как есть.
            None => failed_probe(
                "baseline tier",
                &"the tier is unavailable without a named requirement",
            ),
        },
    }
}

#[cfg(test)]
mod tests;
