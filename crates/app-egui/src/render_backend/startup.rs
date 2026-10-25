//! Старт рендера: выбор семейства, создание и фолбэк при ошибке создания.
//!
//! Склеивает чистый выбор ([`select_renderer`]) с реальностью: пробы и фабрики приходят
//! через [`StartupCandidates`] (в production — каталог платформы, в тестах — fake), а
//! метка незавершённого старта — через [`UnfinishedStartupTracker`] (в production — файл,
//! которым владеет `app_shell`). Модуль не знает ни wgpu, ни GLES2, ни файловой системы.
//!
//! Порядок: метка «пробуем X» → проба X (для каждого кандидата выбора) → метка «создаём X» →
//! создание. Ошибка создания — обработанный
//! сбой, а не падение процесса: метка снимается, семейство помечается как упавшее в ЭТОМ запуске
//! и выбор повторяется без него (решение 39). Каждая попытка исключает ещё одно семейство,
//! поэтому цикл конечен. Фатально только когда не осталось ни одного годного кандидата.

use std::collections::HashMap;

use fastiplayer_config::RenderProfile;
use render_shell_api::{
    RendererCreateError, RendererCreateStage, RendererFamily, RendererProbe, RendererUnusableReason,
};
use tracing::{info, warn};

use super::report::{RendererStartupReport, describe_rejections};
use super::selection::{RendererSelection, RendererStartupMarker, select_renderer};

/// Что старту нужно от каталога рендеров.
///
/// `Renderer` — то, что отдаёт фабрика: в production `Box<dyn RenderShell>`, в тестах — любой
/// маркер (так фолбэк проверяется без fake-реализации всего контракта рендера).
pub(super) trait StartupCandidates {
    /// Созданный рендер.
    type Renderer;

    /// Семейства каталога в порядке предпочтения платформы.
    fn catalog_order(&self) -> Vec<RendererFamily>;

    /// Проба семейства без создания рендера.
    fn probe(&mut self, family: RendererFamily) -> RendererProbe;

    /// Создаёт рендер семейства.
    fn create(&mut self, family: RendererFamily) -> Result<Self::Renderer, RendererCreateError>;
}

/// Учёт метки «запуск рендера не дошёл до первого кадра».
///
/// Владелец метки (файл, момент удаления после первого кадра) — `app_shell`; старт рендера
/// только читает её и сообщает о начале и об обработанном сбое создания.
pub(crate) trait UnfinishedStartupTracker {
    /// Что говорит метка о прошлом запуске.
    fn unfinished_startup(&self) -> RendererStartupMarker;

    /// Перед пробой семейства: проба поднимает драйвер (Vulkan instance, EGL-контекст) и может
    /// повесить или уронить процесс раньше создания рендера, поэтому метка семейства пишется
    /// ДО пробы. Повторная запись того же семейства — no-op (на старте без фолбэка запись одна).
    fn probe_starting(&mut self, family: RendererFamily);

    /// Перед созданием рендера семейства: записать метку (запуск ещё не завершён).
    fn creation_starting(&mut self, family: RendererFamily);

    /// Создание вернуло ошибку: процесс жив, это не падение — метку снять.
    fn creation_failed(&mut self, family: RendererFamily);
}

/// Созданный рендер и отчёт о том, почему выбран именно он.
pub(crate) struct StartedRenderer<Renderer> {
    /// Рендер, готовый к работе.
    pub(crate) renderer: Renderer,

    /// Нейтральный отчёт выбора (телеметрия, уведомление, будущие настройки).
    pub(crate) report: RendererStartupReport,
}

/// Выбирает и создаёт рендер для старта приложения.
pub(super) fn start_renderer<C: StartupCandidates>(
    profile: RenderProfile,
    candidates: &mut C,
    tracker: &mut dyn UnfinishedStartupTracker,
) -> Result<StartedRenderer<C::Renderer>, RendererCreateError> {
    let catalog_order = candidates.catalog_order();
    let startup_marker = tracker.unfinished_startup();
    // Пробы запоминаются между попытками: каждое семейство пробуется не более одного раза.
    let mut probe_cache: HashMap<RendererFamily, RendererProbe> = HashMap::new();
    let mut creation_failures: Vec<(RendererFamily, RendererCreateError)> = Vec::new();

    loop {
        let selection = select_renderer(profile, &catalog_order, startup_marker, &mut |family| {
            probe_with_failures(
                family,
                &creation_failures,
                &mut probe_cache,
                candidates,
                tracker,
            )
        });
        let (family, reason) = match selection {
            RendererSelection::Create { family, reason } => (family, reason),
            RendererSelection::NoUsableRenderer { rejected } => {
                return Err(no_usable_renderer_error(
                    creation_failures,
                    &describe_rejections(&rejected),
                ));
            }
        };

        tracker.creation_starting(family);
        match candidates.create(family) {
            Ok(renderer) => {
                let report = RendererStartupReport::new(family, reason);
                info!(
                    renderer = family.stable_id(),
                    why = %report.telemetry_text(),
                    "Renderer selected"
                );
                return Ok(StartedRenderer { renderer, report });
            }
            Err(error) => {
                tracker.creation_failed(family);
                warn!(
                    renderer = family.stable_id(),
                    stage = ?error.stage,
                    error = %error.message,
                    "Renderer creation failed; trying the next usable renderer"
                );
                creation_failures.push((family, error));
            }
        }
    }
}

/// Проба семейства с учётом сбоев создания этого запуска и памяти проб.
fn probe_with_failures(
    family: RendererFamily,
    creation_failures: &[(RendererFamily, RendererCreateError)],
    probe_cache: &mut HashMap<RendererFamily, RendererProbe>,
    candidates: &mut impl StartupCandidates,
    tracker: &mut dyn UnfinishedStartupTracker,
) -> RendererProbe {
    if let Some((_, error)) = creation_failures
        .iter()
        .find(|(failed_family, _)| *failed_family == family)
    {
        return RendererProbe::Unusable {
            reason: RendererUnusableReason::CreationFailed(error.clone()),
        };
    }
    probe_cache
        .entry(family)
        .or_insert_with(|| {
            // Метка ДО пробы: падение драйвера внутри пробы должно оставить след.
            tracker.probe_starting(family);
            candidates.probe(family)
        })
        .clone()
}

/// Ошибка старта, когда не осталось годных кандидатов: если рендеры падали при создании —
/// последняя такая ошибка (она точнее), иначе перечень причин негодности.
fn no_usable_renderer_error(
    mut creation_failures: Vec<(RendererFamily, RendererCreateError)>,
    rejection_summary: &str,
) -> RendererCreateError {
    match creation_failures.pop() {
        Some((_, last_error)) => last_error,
        None => RendererCreateError::new(
            RendererCreateStage::AdapterSelection,
            format!("no usable renderer on this machine: {rejection_summary}"),
        ),
    }
}

#[cfg(test)]
#[path = "startup/tests.rs"]
mod tests;
