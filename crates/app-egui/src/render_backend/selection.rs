//! Чистая функция выбора семейства рендера (решения владельца 4–5 плана `user/gles2-renderer/`).
//!
//! Ничего не создаёт и не проверяет железо: на вход — профиль из настроек, пробы кандидатов
//! каталога (в порядке предпочтения платформы) и метка незавершённого прошлого старта; на
//! выход — какое семейство создавать и какое уведомление показать. Поэтому вся политика
//! закреплена таблицей тестов без GPU.
//!
//! Сессия 02: на Linux в каталоге один кандидат (wgpu/Vulkan), проба — «годен» (настоящая
//! проверка аппаратного Vulkan и метка старта появятся в сессии 09). Профиль `OpenGles`
//! по решению владельца ведёт себя как раньше: GLES2 ещё не собран, запускается wgpu, в лог
//! пишется предупреждение.

use fastiplayer_config::RenderProfile;
use render_shell_api::{RendererFamily, RendererProbe, RendererUnusableReason};

/// Метка «прошлый запуск с этим рендером не дошёл до первого кадра».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RendererStartupMarker {
    /// Прошлый запуск завершился штатно (или меток ещё нет).
    NoUnfinishedStartup,

    /// Прошлый запуск с этим семейством упал до первого показанного кадра.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "метку незавершённого старта пишет и читает сессия 09 плана gles2-renderer"
        )
    )]
    UnfinishedStartup(RendererFamily),
}

/// Проба одного кандидата каталога.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CandidateProbe {
    /// Семейство кандидата.
    pub(crate) family: RendererFamily,

    /// Годен ли кандидат на этой машине.
    pub(crate) probe: RendererProbe,
}

/// Что выбрал каталог.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RendererSelection {
    /// Создавать это семейство; `notice` — почему выбор отличается от просьбы пользователя.
    Create {
        /// Семейство для создания.
        family: RendererFamily,

        /// Уведомление о замене (лог сейчас, уведомление пользователю — сессия 09).
        notice: Option<RendererSelectionNotice>,
    },

    /// Ни один кандидат не годен: старт невозможен (как прежнее «графика недоступна»).
    NoUsableRenderer {
        /// Причины по каждому кандидату в порядке каталога.
        rejected: Vec<CandidateProbe>,
    },
}

/// Почему выбран не тот рендер, которого ждал пользователь.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RendererSelectionNotice {
    /// Запрошенного семейства нет в каталоге этой сборки (GLES2 до сессии 09).
    RequestedFamilyNotBuilt {
        /// Что просил профиль.
        requested: RendererFamily,
    },

    /// Предпочтительный кандидат пропущен (не годен или упал прошлый старт).
    PreferredCandidateSkipped {
        /// Пропущенный кандидат и причина.
        skipped: CandidateProbe,
    },

    /// Годен только рендер, упавший на прошлом старте: пробуем его ещё раз.
    RetryingAfterUnfinishedStartup {
        /// Семейство, которое упало в прошлый раз.
        family: RendererFamily,
    },
}

/// Выбирает семейство рендера для старта приложения.
///
/// `candidates` — пробы кандидатов каталога в порядке предпочтения платформы.
pub(crate) fn select_renderer(
    profile: RenderProfile,
    candidates: &[CandidateProbe],
    startup_marker: RendererStartupMarker,
) -> RendererSelection {
    match requested_family(profile) {
        Some(requested) => select_requested_family(requested, candidates),
        None => select_automatically(candidates, startup_marker),
    }
}

/// Семейство, которое профиль требует явно; `None` — автовыбор.
const fn requested_family(profile: RenderProfile) -> Option<RendererFamily> {
    match profile {
        RenderProfile::Auto => None,
        RenderProfile::Vulkan => Some(RendererFamily::WgpuVulkan),
        RenderProfile::OpenGles => Some(RendererFamily::Gles2),
    }
}

/// Ручной выбор: именно это семейство, без молчаливой замены.
///
/// Исключение — семейства нет в сборке: тогда берётся первый годный кандидат с
/// уведомлением (решение владельца в сессии 02 для `opengles` до сессии 09).
fn select_requested_family(
    requested: RendererFamily,
    candidates: &[CandidateProbe],
) -> RendererSelection {
    match candidates
        .iter()
        .find(|candidate| candidate.family == requested)
    {
        Some(candidate) if candidate.probe == RendererProbe::Usable => RendererSelection::Create {
            family: requested,
            notice: None,
        },
        Some(candidate) => RendererSelection::NoUsableRenderer {
            rejected: vec![candidate.clone()],
        },
        None => match first_usable(candidates) {
            Some(family) => RendererSelection::Create {
                family,
                notice: Some(RendererSelectionNotice::RequestedFamilyNotBuilt { requested }),
            },
            None => RendererSelection::NoUsableRenderer {
                rejected: candidates.to_vec(),
            },
        },
    }
}

/// Автовыбор: первый годный кандидат, не упавший на прошлом старте.
///
/// Если годен только упавший кандидат, он всё равно выбирается (с уведомлением): без рендера
/// приложение не стартует вовсе, а метка могла остаться от посторонней причины.
fn select_automatically(
    candidates: &[CandidateProbe],
    startup_marker: RendererStartupMarker,
) -> RendererSelection {
    let crashed_family = match startup_marker {
        RendererStartupMarker::NoUnfinishedStartup => None,
        RendererStartupMarker::UnfinishedStartup(family) => Some(family),
    };
    let preferred_position = candidates.iter().position(|candidate| {
        candidate.probe == RendererProbe::Usable && Some(candidate.family) != crashed_family
    });
    if let Some(position) = preferred_position {
        // Уведомление — о первом пропущенном кандидате перед выбранным (если был).
        let notice = candidates[..position].first().map(|skipped| {
            RendererSelectionNotice::PreferredCandidateSkipped {
                skipped: skipped_with_reason(skipped, crashed_family),
            }
        });
        return RendererSelection::Create {
            family: candidates[position].family,
            notice,
        };
    }
    match crashed_family.filter(|family| is_usable(candidates, *family)) {
        Some(family) => RendererSelection::Create {
            family,
            notice: Some(RendererSelectionNotice::RetryingAfterUnfinishedStartup { family }),
        },
        None => RendererSelection::NoUsableRenderer {
            rejected: candidates.to_vec(),
        },
    }
}

/// Пропущенный кандидат с причиной пропуска (годный, но упавший — `PreviousStartupCrashed`).
fn skipped_with_reason(
    candidate: &CandidateProbe,
    crashed_family: Option<RendererFamily>,
) -> CandidateProbe {
    match &candidate.probe {
        RendererProbe::Usable if Some(candidate.family) == crashed_family => CandidateProbe {
            family: candidate.family,
            probe: RendererProbe::Unusable {
                reason: RendererUnusableReason::PreviousStartupCrashed,
            },
        },
        _ => candidate.clone(),
    }
}

/// Первый годный кандидат каталога.
fn first_usable(candidates: &[CandidateProbe]) -> Option<RendererFamily> {
    candidates
        .iter()
        .find(|candidate| candidate.probe == RendererProbe::Usable)
        .map(|candidate| candidate.family)
}

/// Годен ли кандидат этого семейства по пробе.
fn is_usable(candidates: &[CandidateProbe], family: RendererFamily) -> bool {
    candidates
        .iter()
        .any(|candidate| candidate.family == family && candidate.probe == RendererProbe::Usable)
}

#[cfg(test)]
#[path = "selection/tests.rs"]
mod tests;
