//! Чистая функция выбора семейства рендера (решения владельца 4–5, 35, 39–40 плана
//! `user/gles2-renderer/`).
//!
//! Ничего не создаёт и не трогает железо сама: на вход — профиль из настроек, семейства
//! каталога в порядке предпочтения платформы, метка незавершённого прошлого старта и
//! функция-проба; на выход — какое семейство создавать и по какой причине. Пробы вызываются
//! ЛЕНИВО и не больше одного раза на семейство: если первый кандидат годен и не помечен,
//! проба второго не выполняется (старт на машине с хорошим Vulkan не дорожает). Поэтому вся
//! политика закреплена таблицей тестов без GPU.
//!
//! Политика:
//! - `Auto`: первый годный кандидат, не упавший на прошлом старте. Негодный по пробе
//!   пропускается молча (решение 40), упавший по метке или при создании — с уведомлением.
//!   Если годен только упавший кандидат, он пробуется ещё раз (метка могла остаться от
//!   посторонней причины, а без рендера приложение не стартует вовсе).
//! - Явный профиль: именно это семейство. Если оно негодно (39) или прошлый старт с ним не
//!   завершился (35) — ЭТОТ запуск идёт на следующем годном кандидате с уведомлением;
//!   настройка не меняется. Фатально только если не годен НИ ОДИН кандидат.
//! - Создание, упавшее в этом запуске, приходит сюда как проба `CreationFailed`: выбор
//!   повторяется без упавшего семейства (см. `startup.rs`).

use fastiplayer_config::RenderProfile;
use render_shell_api::{RendererFamily, RendererProbe, RendererUnusableReason};

/// Метка «прошлый запуск с этим рендером не дошёл до первого кадра».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RendererStartupMarker {
    /// Прошлый запуск завершился штатно (или меток нет).
    NoUnfinishedStartup,

    /// Прошлый запуск с этим семейством упал до первого показанного кадра.
    UnfinishedStartup(RendererFamily),
}

impl RendererStartupMarker {
    /// Семейство, упавшее на прошлом старте, если метка есть.
    const fn crashed_family(self) -> Option<RendererFamily> {
        match self {
            Self::NoUnfinishedStartup => None,
            Self::UnfinishedStartup(family) => Some(family),
        }
    }
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
    /// Создавать это семейство по этой причине.
    Create {
        /// Семейство для создания.
        family: RendererFamily,

        /// Почему выбрано именно оно (для лога, телеметрии и уведомления).
        reason: SelectionReason,
    },

    /// Ни один кандидат не годен: старт невозможен (как прежнее «графика недоступна»).
    NoUsableRenderer {
        /// Причины по каждому кандидату в порядке каталога.
        rejected: Vec<CandidateProbe>,
    },
}

/// Почему выбрано это семейство. Часть причин — штатный выбор (молча), часть — сбой, о
/// котором пользователь должен узнать ([`SelectionReason::needs_user_notice`], решение 40).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelectionReason {
    /// `Auto`: первый кандидат каталога годен.
    AutoFirstChoice,

    /// `Auto`: кандидаты раньше выбранного не годны по пробе (штатно, без уведомления).
    AutoSkippedUnusable {
        /// Пропущенные кандидаты и причины их негодности.
        skipped: Vec<CandidateProbe>,
    },

    /// Явный профиль выполнен как написано.
    ProfileHonoured,

    /// `Auto`: прошлый старт с этим семейством не завершился, берём следующего годного.
    PreviousStartupUnfinishedInAuto {
        /// Семейство, упавшее в прошлый раз.
        crashed: RendererFamily,
    },

    /// Явный профиль, но прошлый старт с этим семейством не завершился (решение 35).
    RequestedStartupUnfinished {
        /// Семейство, которое просил профиль (и которое упало).
        requested: RendererFamily,
    },

    /// Годно только упавшее на прошлом старте семейство: пробуем его ещё раз.
    RetryingAfterUnfinishedStartup,

    /// Создание предыдущих кандидатов в этом запуске упало.
    CreationFailed {
        /// Упавшие кандидаты с ошибками создания.
        failed: Vec<CandidateProbe>,
    },

    /// Явный профиль невыполним на этой машине (решение 39).
    RequestedUnusable {
        /// Семейство, которое просил профиль.
        requested: RendererFamily,

        /// Почему его нельзя использовать.
        reason: RendererUnusableReason,
    },

    /// Просили семейство, которого нет в этой сборке.
    RequestedNotBuilt {
        /// Семейство, которое просил профиль.
        requested: RendererFamily,
    },
}

impl SelectionReason {
    /// Нужно ли показать пользователю уведомление «до ×» (решение 40: только при сбоях).
    pub(crate) const fn needs_user_notice(&self) -> bool {
        match self {
            Self::AutoFirstChoice | Self::AutoSkippedUnusable { .. } | Self::ProfileHonoured => {
                false
            }
            Self::PreviousStartupUnfinishedInAuto { .. }
            | Self::RequestedStartupUnfinished { .. }
            | Self::RetryingAfterUnfinishedStartup
            | Self::CreationFailed { .. }
            | Self::RequestedUnusable { .. }
            | Self::RequestedNotBuilt { .. } => true,
        }
    }
}

/// Результаты проб с памятью: каждое семейство пробуется не более одного раза за выбор.
struct ProbeLedger<'probe> {
    /// Семейства каталога в порядке предпочтения.
    catalog_order: &'probe [RendererFamily],

    /// Функция-проба; вызывается только когда результат нужен решению.
    probe_family: &'probe mut dyn FnMut(RendererFamily) -> RendererProbe,

    /// Уже полученные результаты.
    results: Vec<CandidateProbe>,
}

impl<'probe> ProbeLedger<'probe> {
    fn new(
        catalog_order: &'probe [RendererFamily],
        probe_family: &'probe mut dyn FnMut(RendererFamily) -> RendererProbe,
    ) -> Self {
        Self {
            catalog_order,
            probe_family,
            results: Vec::new(),
        }
    }

    /// Проба семейства (из памяти или новая).
    fn probe(&mut self, family: RendererFamily) -> RendererProbe {
        if let Some(known) = self.results.iter().find(|result| result.family == family) {
            return known.probe.clone();
        }
        let probe = (self.probe_family)(family);
        self.results.push(CandidateProbe {
            family,
            probe: probe.clone(),
        });
        probe
    }

    /// Годно ли семейство по пробе.
    fn is_usable(&mut self, family: RendererFamily) -> bool {
        self.probe(family) == RendererProbe::Usable
    }

    /// Пробы всех кандидатов в порядке каталога (для отчёта «ничего не годно»).
    fn probe_everything(&mut self) -> Vec<CandidateProbe> {
        let catalog_order = self.catalog_order;
        catalog_order
            .iter()
            .map(|family| CandidateProbe {
                family: *family,
                probe: self.probe(*family),
            })
            .collect()
    }
}

/// Выбирает семейство рендера для старта приложения.
///
/// `catalog_order` — семейства каталога в порядке предпочтения платформы;
/// `probe_family` — проба семейства, вызывается лениво и не более одного раза на семейство.
pub(crate) fn select_renderer(
    profile: RenderProfile,
    catalog_order: &[RendererFamily],
    startup_marker: RendererStartupMarker,
    probe_family: &mut dyn FnMut(RendererFamily) -> RendererProbe,
) -> RendererSelection {
    let mut ledger = ProbeLedger::new(catalog_order, probe_family);
    let crashed_family = startup_marker.crashed_family();
    match requested_family(profile) {
        None => select_automatically(&mut ledger, catalog_order, crashed_family),
        Some(requested) => select_requested_family(&mut ledger, requested, crashed_family),
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

/// Результат обхода кандидатов автовыбором.
enum AutoPick {
    /// Годный кандидат нашёлся; `skipped` — кандидаты раньше него.
    Picked {
        family: RendererFamily,
        skipped: Vec<CandidateProbe>,
    },

    /// Годен только упавший на прошлом старте: пробуем его снова.
    RetryCrashed(RendererFamily),

    /// Годных нет.
    Nothing,
}

/// Обходит `candidates` по порядку: первый годный, не упавший на прошлом старте.
fn pick_automatically(
    ledger: &mut ProbeLedger<'_>,
    candidates: &[RendererFamily],
    crashed_family: Option<RendererFamily>,
) -> AutoPick {
    let mut skipped = Vec::new();
    for &family in candidates {
        if Some(family) == crashed_family {
            // Упавшего не пробуем сразу: проба нужна, только если больше выбирать не из чего.
            skipped.push(CandidateProbe {
                family,
                probe: RendererProbe::Unusable {
                    reason: RendererUnusableReason::PreviousStartupCrashed,
                },
            });
            continue;
        }
        let probe = ledger.probe(family);
        if probe == RendererProbe::Usable {
            return AutoPick::Picked { family, skipped };
        }
        skipped.push(CandidateProbe { family, probe });
    }
    match crashed_family.filter(|family| candidates.contains(family)) {
        Some(family) if ledger.is_usable(family) => AutoPick::RetryCrashed(family),
        _ => AutoPick::Nothing,
    }
}

/// Автовыбор (`Auto`).
fn select_automatically(
    ledger: &mut ProbeLedger<'_>,
    catalog_order: &[RendererFamily],
    crashed_family: Option<RendererFamily>,
) -> RendererSelection {
    match pick_automatically(ledger, catalog_order, crashed_family) {
        AutoPick::Picked { family, skipped } => RendererSelection::Create {
            family,
            reason: automatic_reason(skipped),
        },
        AutoPick::RetryCrashed(family) => RendererSelection::Create {
            family,
            reason: SelectionReason::RetryingAfterUnfinishedStartup,
        },
        AutoPick::Nothing => RendererSelection::NoUsableRenderer {
            rejected: ledger.probe_everything(),
        },
    }
}

/// Причина автовыбора по пропущенным кандидатам. Приоритет: сбой создания в этом запуске,
/// затем метка прошлого старта, затем штатный пропуск негодных.
fn automatic_reason(skipped: Vec<CandidateProbe>) -> SelectionReason {
    let failed_creation: Vec<CandidateProbe> = skipped
        .iter()
        .filter(|candidate| is_creation_failure(&candidate.probe))
        .cloned()
        .collect();
    if !failed_creation.is_empty() {
        return SelectionReason::CreationFailed {
            failed: failed_creation,
        };
    }
    let crashed = skipped.iter().find(|candidate| {
        candidate.probe
            == RendererProbe::Unusable {
                reason: RendererUnusableReason::PreviousStartupCrashed,
            }
    });
    if let Some(crashed) = crashed {
        return SelectionReason::PreviousStartupUnfinishedInAuto {
            crashed: crashed.family,
        };
    }
    if skipped.is_empty() {
        SelectionReason::AutoFirstChoice
    } else {
        SelectionReason::AutoSkippedUnusable { skipped }
    }
}

/// Проба говорит «создание упало в этом запуске».
fn is_creation_failure(probe: &RendererProbe) -> bool {
    matches!(
        probe,
        RendererProbe::Unusable {
            reason: RendererUnusableReason::CreationFailed(_)
        }
    )
}

/// Ручной выбор: именно это семейство; невыполнимый профиль не фатален, если есть другой
/// годный кандидат (решения 35 и 39) — на этот запуск, настройка не меняется.
fn select_requested_family(
    ledger: &mut ProbeLedger<'_>,
    requested: RendererFamily,
    crashed_family: Option<RendererFamily>,
) -> RendererSelection {
    let catalog_order = ledger.catalog_order;
    if !catalog_order.contains(&requested) {
        return match pick_automatically(ledger, catalog_order, crashed_family) {
            AutoPick::Picked { family, .. } => RendererSelection::Create {
                family,
                reason: SelectionReason::RequestedNotBuilt { requested },
            },
            AutoPick::RetryCrashed(family) => RendererSelection::Create {
                family,
                reason: SelectionReason::RetryingAfterUnfinishedStartup,
            },
            AutoPick::Nothing => RendererSelection::NoUsableRenderer {
                rejected: ledger.probe_everything(),
            },
        };
    }
    let others: Vec<RendererFamily> = catalog_order
        .iter()
        .copied()
        .filter(|family| *family != requested)
        .collect();

    if crashed_family == Some(requested) {
        // Решение 35: прошлый старт с запрошенным семейством не завершился.
        if let AutoPick::Picked { family, .. } = pick_automatically(ledger, &others, None) {
            return RendererSelection::Create {
                family,
                reason: SelectionReason::RequestedStartupUnfinished { requested },
            };
        }
        // Больше выбирать не из чего: пробуем запрошенное ещё раз, если оно годно.
        return if ledger.is_usable(requested) {
            RendererSelection::Create {
                family: requested,
                reason: SelectionReason::RetryingAfterUnfinishedStartup,
            }
        } else {
            RendererSelection::NoUsableRenderer {
                rejected: ledger.probe_everything(),
            }
        };
    }

    match ledger.probe(requested) {
        RendererProbe::Usable => RendererSelection::Create {
            family: requested,
            reason: SelectionReason::ProfileHonoured,
        },
        RendererProbe::Unusable { reason } => {
            // Решение 39: запрошенное негодно — следующий годный кандидат на этот запуск.
            match pick_automatically(ledger, &others, crashed_family) {
                AutoPick::Picked { family, .. } => RendererSelection::Create {
                    family,
                    reason: SelectionReason::RequestedUnusable { requested, reason },
                },
                AutoPick::RetryCrashed(family) => RendererSelection::Create {
                    family,
                    reason: SelectionReason::RetryingAfterUnfinishedStartup,
                },
                AutoPick::Nothing => RendererSelection::NoUsableRenderer {
                    rejected: ledger.probe_everything(),
                },
            }
        }
    }
}

#[cfg(test)]
#[path = "selection/tests.rs"]
mod tests;
