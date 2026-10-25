//! Нейтральный отчёт о выборе рендера при старте: кто выбран и почему.
//!
//! Отчёт — единственное, что приложение знает о выборе: семейство (`RendererFamily`) и
//! причина ([`SelectionReason`]). Конкретные типы рендеров сюда не попадают, поэтому им
//! безопасно пользоваться состоянию приложения (строка телеметрии, уведомления) и будущим
//! владельцам настроек (сессия 09b): семейство активного рендера берётся отсюда, а не из
//! типа рендера.
//!
//! Тексты для пользователя — английские (`mem:app-egui/user-facing-language`); решение
//! «показывать ли уведомление» принимает [`SelectionReason::needs_user_notice`] (решение
//! владельца 40: обычный выбор `Auto` молчит, уведомления — только при сбоях).

use render_shell_api::{RendererFamily, RendererUnusableReason};

use super::selection::{CandidateProbe, SelectionReason};

/// Предел длины сообщения драйвера в тексте уведомления (остальное — в логе).
const DRIVER_MESSAGE_PREVIEW_CHARS: usize = 160;

/// Итог выбора рендера на старте приложения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RendererStartupReport {
    /// Семейство созданного рендера.
    family: RendererFamily,

    /// Почему выбрано именно оно.
    reason: SelectionReason,
}

impl RendererStartupReport {
    /// Отчёт о выборе `family` по причине `reason`.
    pub(crate) fn new(family: RendererFamily, reason: SelectionReason) -> Self {
        Self { family, reason }
    }

    /// Семейство активного рендера.
    pub(crate) fn family(&self) -> RendererFamily {
        self.family
    }

    /// Причина выбора.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "сессия 09b: владельцы настроек читают причину выбора отсюда"
        )
    )]
    pub(crate) fn reason(&self) -> &SelectionReason {
        &self.reason
    }

    /// Текст строки панели телеметрии после `Renderer: `, например
    /// `OpenGL ES 2.0 — Auto: no hardware Vulkan adapter`.
    pub(crate) fn telemetry_text(&self) -> String {
        format!(
            "{} — {}",
            family_title(self.family),
            reason_summary(&self.reason)
        )
    }

    /// Уведомление «до ×» для пользователя; `None`, если выбор штатный (решение 40).
    pub(crate) fn user_notice(&self) -> Option<String> {
        if !self.reason.needs_user_notice() {
            return None;
        }
        Some(notice_text(self.family, &self.reason))
    }
}

/// Название семейства для телеметрии.
const fn family_title(family: RendererFamily) -> &'static str {
    match family {
        RendererFamily::WgpuVulkan => "Vulkan (wgpu)",
        RendererFamily::Gles2 => "OpenGL ES 2.0",
    }
}

/// Название графического API семейства для фраз вроде «the Vulkan renderer».
const fn api_name(family: RendererFamily) -> &'static str {
    match family {
        RendererFamily::WgpuVulkan => "Vulkan",
        RendererFamily::Gles2 => "OpenGL ES 2.0",
    }
}

/// Одной строкой: почему ни один кандидат не годен (для фатальной ошибки и лога).
pub(super) fn describe_rejections(rejected: &[CandidateProbe]) -> String {
    rejected
        .iter()
        .map(|candidate| {
            format!(
                "{}: {}",
                family_title(candidate.family),
                unusable_text(candidate)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Короткая причина для строки телеметрии.
fn reason_summary(reason: &SelectionReason) -> String {
    match reason {
        SelectionReason::AutoFirstChoice => "Auto".to_owned(),
        SelectionReason::AutoSkippedUnusable { skipped } => match skipped.first() {
            Some(first) => format!("Auto: {}", unusable_text(first)),
            None => "Auto".to_owned(),
        },
        SelectionReason::ProfileHonoured => "chosen in Settings".to_owned(),
        SelectionReason::PreviousStartupUnfinishedInAuto { crashed } => unfinished_text(*crashed),
        SelectionReason::RequestedStartupUnfinished { requested } => format!(
            "{} (Settings still say {})",
            unfinished_text(*requested),
            api_name(*requested)
        ),
        SelectionReason::RetryingAfterUnfinishedStartup => {
            "retrying after an unfinished start".to_owned()
        }
        SelectionReason::CreationFailed { failed } => match failed.first() {
            Some(first) => unusable_text(first),
            None => "renderer failed to start".to_owned(),
        },
        SelectionReason::RequestedUnusable { requested, reason } => format!(
            "{} (Settings still say {})",
            unusable_reason_text(*requested, reason),
            api_name(*requested)
        ),
        SelectionReason::RequestedNotBuilt { requested } => {
            format!("{} is not built into this version", api_name(*requested))
        }
    }
}

/// «Vulkan renderer did not finish starting last time».
fn unfinished_text(family: RendererFamily) -> String {
    format!(
        "{} renderer did not finish starting last time",
        api_name(family)
    )
}

/// Почему пропущенный кандидат негоден.
fn unusable_text(candidate: &CandidateProbe) -> String {
    match &candidate.probe {
        render_shell_api::RendererProbe::Usable => {
            format!("{} is usable", api_name(candidate.family))
        }
        render_shell_api::RendererProbe::Unusable { reason } => {
            unusable_reason_text(candidate.family, reason)
        }
    }
}

/// Человекочитаемая причина негодности семейства.
fn unusable_reason_text(family: RendererFamily, reason: &RendererUnusableReason) -> String {
    let api = api_name(family);
    match reason {
        RendererUnusableReason::NoHardwareAdapter => format!("no hardware {api} adapter"),
        RendererUnusableReason::SoftwareAdapterOnly => {
            format!("only a software {api} adapter is available")
        }
        RendererUnusableReason::MissingRequiredFeature(feature) => {
            format!("{api} lacks {}", feature.specification_name())
        }
        RendererUnusableReason::PreviousStartupCrashed => unfinished_text(family),
        RendererUnusableReason::CreationFailed(error) => format!(
            "the {api} renderer could not start ({})",
            preview(&error.message)
        ),
    }
}

/// Первая строка сообщения драйвера, обрезанная по символам.
fn preview(message: &str) -> String {
    let first_line = message.lines().next().unwrap_or_default().trim();
    if first_line.chars().count() <= DRIVER_MESSAGE_PREVIEW_CHARS {
        return first_line.to_owned();
    }
    let cut: String = first_line
        .chars()
        .take(DRIVER_MESSAGE_PREVIEW_CHARS)
        .collect();
    format!("{cut}…")
}

/// Полный текст уведомления: что случилось и как вернуть прежнее.
fn notice_text(selected: RendererFamily, reason: &SelectionReason) -> String {
    let selected_api = api_name(selected);
    match reason {
        SelectionReason::PreviousStartupUnfinishedInAuto { crashed } => format!(
            "The {crashed_api} renderer did not finish starting last time, so {selected_api} is used for this launch. Restart the player to try {crashed_api} again.",
            crashed_api = api_name(*crashed)
        ),
        SelectionReason::RequestedStartupUnfinished { requested } => format!(
            "Settings still say {requested_api}, but it did not finish starting last time. {selected_api} is used for this launch; the setting is unchanged, so the next launch tries {requested_api} again.",
            requested_api = api_name(*requested)
        ),
        SelectionReason::RetryingAfterUnfinishedStartup => format!(
            "The {selected_api} renderer did not finish starting last time and no other renderer is available, so the player is trying it again."
        ),
        SelectionReason::CreationFailed { failed } => {
            let failed_api = failed
                .first()
                .map_or("The previous", |candidate| api_name(candidate.family));
            format!(
                "The {failed_api} renderer could not start, so {selected_api} is used for this launch. Restart the player to try again."
            )
        }
        SelectionReason::RequestedUnusable { requested, reason } => format!(
            "Settings still say {requested_api}, but {why}. {selected_api} is used for this launch; the setting is unchanged. To use {selected_api} permanently, change the renderer in Settings.",
            requested_api = api_name(*requested),
            why = unusable_reason_text(*requested, reason)
        ),
        SelectionReason::RequestedNotBuilt { requested } => format!(
            "The {requested_api} renderer is not available in this version; {selected_api} is used instead.",
            requested_api = api_name(*requested)
        ),
        // Штатные причины уведомления не имеют; сюда не попадают (см. `user_notice`).
        SelectionReason::AutoFirstChoice
        | SelectionReason::AutoSkippedUnusable { .. }
        | SelectionReason::ProfileHonoured => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use render_shell_api::{
        RendererCreateError, RendererCreateStage, RendererFamily, RendererProbe,
        RendererUnusableReason,
    };

    use super::RendererStartupReport;
    use crate::render_backend::selection::{CandidateProbe, SelectionReason};

    const WGPU: RendererFamily = RendererFamily::WgpuVulkan;
    const GLES2: RendererFamily = RendererFamily::Gles2;

    fn skipped(family: RendererFamily, reason: RendererUnusableReason) -> CandidateProbe {
        CandidateProbe {
            family,
            probe: RendererProbe::Unusable { reason },
        }
    }

    /// Строка телеметрии называет семейство и причину так, как обещано владельцу.
    #[test]
    fn telemetry_text_names_the_family_and_the_reason() {
        let cases = [
            (
                RendererStartupReport::new(WGPU, SelectionReason::AutoFirstChoice),
                "Vulkan (wgpu) — Auto",
            ),
            (
                RendererStartupReport::new(
                    GLES2,
                    SelectionReason::AutoSkippedUnusable {
                        skipped: vec![skipped(WGPU, RendererUnusableReason::NoHardwareAdapter)],
                    },
                ),
                "OpenGL ES 2.0 — Auto: no hardware Vulkan adapter",
            ),
            (
                RendererStartupReport::new(
                    GLES2,
                    SelectionReason::PreviousStartupUnfinishedInAuto { crashed: WGPU },
                ),
                "OpenGL ES 2.0 — Vulkan renderer did not finish starting last time",
            ),
            (
                RendererStartupReport::new(
                    GLES2,
                    SelectionReason::AutoSkippedUnusable {
                        skipped: vec![skipped(WGPU, RendererUnusableReason::SoftwareAdapterOnly)],
                    },
                ),
                "OpenGL ES 2.0 — Auto: only a software Vulkan adapter is available",
            ),
        ];
        for (report, expected) in cases {
            assert_eq!(report.telemetry_text(), expected);
        }
    }

    /// Обычный выбор молчит; каждый сбой даёт уведомление, где названо, что случилось,
    /// и что делать.
    #[test]
    fn only_failures_produce_a_user_notice_with_the_way_back() {
        let quiet = [
            RendererStartupReport::new(WGPU, SelectionReason::AutoFirstChoice),
            RendererStartupReport::new(WGPU, SelectionReason::ProfileHonoured),
            RendererStartupReport::new(
                GLES2,
                SelectionReason::AutoSkippedUnusable {
                    skipped: vec![skipped(WGPU, RendererUnusableReason::NoHardwareAdapter)],
                },
            ),
        ];
        for report in quiet {
            assert_eq!(report.user_notice(), None, "{report:?}");
        }

        let explicit_unfinished = RendererStartupReport::new(
            GLES2,
            SelectionReason::RequestedStartupUnfinished { requested: WGPU },
        )
        .user_notice()
        .expect("решение 35 требует уведомления");
        assert!(explicit_unfinished.contains("Settings still say Vulkan"));
        assert!(explicit_unfinished.contains("next launch tries Vulkan again"));

        let unusable_explicit = RendererStartupReport::new(
            GLES2,
            SelectionReason::RequestedUnusable {
                requested: WGPU,
                reason: RendererUnusableReason::SoftwareAdapterOnly,
            },
        )
        .user_notice()
        .expect("решение 39 требует уведомления");
        assert!(unusable_explicit.contains("Settings still say Vulkan"));
        assert!(unusable_explicit.contains("only a software Vulkan adapter"));
        assert!(unusable_explicit.contains("OpenGL ES 2.0 is used for this launch"));

        let creation_failed = RendererStartupReport::new(
            GLES2,
            SelectionReason::CreationFailed {
                failed: vec![skipped(
                    WGPU,
                    RendererUnusableReason::CreationFailed(RendererCreateError::new(
                        RendererCreateStage::DeviceCreation,
                        "driver refused",
                    )),
                )],
            },
        )
        .user_notice()
        .expect("сбой создания требует уведомления");
        assert!(creation_failed.contains("The Vulkan renderer could not start"));
        assert!(creation_failed.contains("Restart the player to try again"));

        let crashed_in_auto = RendererStartupReport::new(
            GLES2,
            SelectionReason::PreviousStartupUnfinishedInAuto { crashed: WGPU },
        )
        .user_notice()
        .expect("метка требует уведомления");
        assert!(crashed_in_auto.contains("did not finish starting last time"));
        assert!(crashed_in_auto.contains("Restart the player to try Vulkan again"));
    }

    /// Длинное сообщение драйвера обрезается по символам (не по байтам) и до первой строки.
    #[test]
    fn driver_message_in_telemetry_is_one_short_line() {
        let long_message = format!("{}\nsecond line", "ж".repeat(400));
        let report = RendererStartupReport::new(
            GLES2,
            SelectionReason::CreationFailed {
                failed: vec![skipped(
                    WGPU,
                    RendererUnusableReason::CreationFailed(RendererCreateError::new(
                        RendererCreateStage::DeviceCreation,
                        long_message,
                    )),
                )],
            },
        );

        let text = report.telemetry_text();

        assert!(!text.contains("second line"));
        assert!(text.contains(&format!("{}…", "ж".repeat(160))));
        assert!(!text.contains(&"ж".repeat(161)));
    }
}
