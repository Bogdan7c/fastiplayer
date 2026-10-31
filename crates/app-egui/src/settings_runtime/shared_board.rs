//! «Общая доска» настроек: применение изменений, которые записала соседняя копия плеера
//! (решения владельца 135 и 137).
//!
//! Обычное окно и окна браузерного режима пишут один `config.toml` («только свои тронутые
//! ключи» под замком — `fastiplayer-settings::shared_persist`). Здесь — вторая половина:
//! копия узнаёт, что файл изменился (сигнал `config_file_watch`), перечитывает его,
//! сравнивает со своим committed и применяет разницу тем же конвейером, что Apply, но без
//! записи (`settings-core::begin_adopt_external_settings`).
//!
//! ```text
//! сигнал слежения ──► ждём своей очереди (свой Apply / runtime-коммит в полёте — ждём)
//!   └► отпечаток файла уже разобран? ── да ──► тихо (не зацикливаемся на том же содержимом)
//!   └► read_shared_config_at:
//!        Missing    ── тихо
//!        Unreadable ── одно уведомление на новую проблему, ничего не применяем и не пишем
//!        Newer      ── то же своим текстом
//!        Parsed     ── дописать несохранённое (A2) → диф без «запоминаемого состояния»
//!                      → begin_adopt (Pending — опрос каждый кадр, Busy — ждём своего)
//!                      → Adopted: snapshot потребителям; отказ/откат: уведомление + отпечаток
//! ```
//!
//! Инварианты:
//! - своя запись даёт пустой диф (`NothingToAdopt`): ни уведомлений, ни транзакций;
//! - «запоминаемое состояние» (громкость, mute, субтитры вкл/выкл, ширина сайдбара, окно
//!   браузерного режима) на лету не применяется — решает `fastiplayer-settings::peer_adoption`;
//! - принятие не пишет файл, не показывает «Settings saved» и не зовёт хуки после записи
//!   (громкость UX-11 и т. п.);
//! - во время своего Apply чужое ждёт его конца; во время принятия свой Apply ждёт в
//!   `Scheduled` (`stepwise_apply.rs`), отложенные runtime-коммиты — как во время Apply.

use std::time::{Duration, Instant};

use fastiplayer_config::{
    ConfigSavePolicy, ConfigSessionOnlyReason, SharedConfigProblem, SharedConfigRead,
    read_shared_config_at,
};
use fastiplayer_settings::{UnsavedChangesSave, adoptable_peer_changes};
use settings_core::{
    BeginExternalAdoption, ExternalAdoptionOutcome, ExternalAdoptionReport, PollExternalAdoption,
};

use super::transaction::SettingsRuntimeApplyDelegate;
use super::*;
use crate::config_file_watch::{
    ConfigFileChange, ConfigFileChangeReceiver, ConfigFileWatch, start_config_file_watch,
};

mod fingerprint;
mod notice;
mod save_notice;

use fingerprint::ConfigFileFingerprint;
pub(crate) use notice::SettingsFileNotice;
pub(super) use save_notice::{PersistNoticeRoute, SaveDeferralNotices};

/// Через сколько повторить принятие, если плеер был занят (перемотка, скраб, пересборка).
///
/// Это не ошибка для человека: значения соседа доедут, как только плеер освободится.
/// Повтор раз в четверть секунды не нагружает UI-поток (одно чтение маленького файла).
const RETRY_AFTER_RUNTIME_BUSY: Duration = Duration::from_millis(250);

/// Почему файл на диске нельзя принять (ключ «уже сообщили»).
#[derive(Debug, Clone, PartialEq, Eq)]
enum SettingsFileProblem {
    Unreadable(SharedConfigProblem),
    Newer { found: i64 },
}

impl SettingsFileProblem {
    /// Проблема, о которой уже сообщило окно при старте (вердикт загрузки config-а):
    /// повторять её первым же сигналом слежения не нужно.
    fn announced_at_startup(save_policy: &ConfigSavePolicy) -> Option<Self> {
        let ConfigSavePolicy::SessionOnly(reason) = save_policy else {
            return None;
        };
        match reason {
            ConfigSessionOnlyReason::NewerSchemaVersion { found } => {
                Some(Self::Newer { found: *found })
            }
            ConfigSessionOnlyReason::ConfigFileUnreadable { error_kind } => {
                Some(Self::Unreadable(SharedConfigProblem::ReadFailed {
                    error_kind: *error_kind,
                }))
            }
            ConfigSessionOnlyReason::ConfigPathIsNotFile => {
                Some(Self::Unreadable(SharedConfigProblem::NotRegularFile))
            }
            ConfigSessionOnlyReason::BrokenConfigLeftToRegularPlayer(problem) => Some(
                Self::Unreadable(SharedConfigProblem::Broken(problem.clone())),
            ),
            // Остальные причины не про содержимое файла (замок, каталог, запись defaults).
            _ => None,
        }
    }

    fn notice(&self) -> SettingsFileNotice {
        match self {
            Self::Unreadable(_) => SettingsFileNotice::UnreadableFile,
            Self::Newer { .. } => SettingsFileNotice::NewerFileVersion,
        }
    }
}

/// Сигнал получен, но разобрать его пока нельзя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AwaitingTurn {
    /// Не раньше этого момента (плеер был занят); `None` — как только освободится очередь.
    retry_not_before: Option<Instant>,
}

/// Принятие, которое ждёт маршрут (например, пересборку источника).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AdoptionAttempt {
    /// Отпечаток файла, из которого взяты значения.
    fingerprint: Option<ConfigFileFingerprint>,
    /// Принимаемые id (для журнала; id — не данные просмотра).
    setting_ids: Vec<SettingId>,
}

/// Состояние доски внутри [`SettingsRuntime`].
pub(super) struct SharedSettingsBoard {
    /// Живое слежение за каталогом config-а (поток останавливается при `Drop`).
    watch: Option<ConfigFileWatch>,
    /// Сторона UI-потока сигнала «файл мог измениться».
    receiver: Option<ConfigFileChangeReceiver>,
    /// Сигнал получен и ждёт своей очереди.
    awaiting_turn: Option<AwaitingTurn>,
    /// Принятие в полёте.
    in_flight: Option<AdoptionAttempt>,
    /// Содержимое, которое уже разобрано без успеха (битое, новее, отказ, откат).
    settled_fingerprint: Option<ConfigFileFingerprint>,
    /// Проблема файла, о которой человеку уже сообщили.
    announced_problem: Option<SettingsFileProblem>,
    /// «Применено, но в файл пока не записано»: одно уведомление на причину.
    save_notices: SaveDeferralNotices,
}

impl SharedSettingsBoard {
    /// Доска без слежения; проблема старта уже показана окном при загрузке.
    pub(super) fn from_save_policy(save_policy: &ConfigSavePolicy) -> Self {
        Self {
            watch: None,
            receiver: None,
            awaiting_turn: None,
            in_flight: None,
            settled_fingerprint: None,
            announced_problem: SettingsFileProblem::announced_at_startup(save_policy),
            save_notices: SaveDeferralNotices::default(),
        }
    }

    /// Учёт уведомлений записи для делегата транзакции (заимствует только доску).
    pub(super) fn save_notices_mut(&mut self) -> &mut SaveDeferralNotices {
        &mut self.save_notices
    }
}

/// Итог шага доски за кадр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(crate) struct SharedBoardFrame {
    /// Состояние настроек изменилось — окно нужно перерисовать.
    pub(crate) needs_redraw: bool,
    /// Уведомление для человека (центр уведомлений окна).
    pub(crate) notice: Option<SettingsFileNotice>,
}

impl SharedBoardFrame {
    const fn idle() -> Self {
        Self {
            needs_redraw: false,
            notice: None,
        }
    }

    const fn changed() -> Self {
        Self {
            needs_redraw: true,
            notice: None,
        }
    }

    const fn with_notice(notice: SettingsFileNotice) -> Self {
        Self {
            needs_redraw: true,
            notice: Some(notice),
        }
    }
}

impl SettingsRuntime {
    /// Начинает следить за своим config-файлом (обычное окно и окно браузерного режима).
    ///
    /// Повторный вызов ничего не делает: слежение одно на копию.
    pub(crate) fn start_watching_config_file(&mut self, wake_port: AppWakePort) {
        if self.shared_board.receiver.is_some() {
            return;
        }
        let (watch, receiver) = start_config_file_watch(&self.config_path, wake_port);
        self.shared_board.watch = watch;
        self.shared_board.receiver = Some(receiver);
    }

    /// Есть ли не разобранный сигнал слежения (пробуждение цикла решает «перерисовать»).
    #[must_use]
    pub(crate) fn has_config_file_change_signal(&self) -> bool {
        self.shared_board
            .receiver
            .as_ref()
            .is_some_and(ConfigFileChangeReceiver::has_pending_change)
    }

    /// Работа доски, ради которой циклу нужно просыпаться (как ради своего Apply).
    pub(super) fn has_shared_board_work(&self) -> bool {
        self.controller.has_pending_external_adoption() || self.shared_board.awaiting_turn.is_some()
    }

    /// Забирает уведомление «применено, но в файл пока не записано» (одно на причину).
    pub(crate) fn take_settings_save_notice(&mut self) -> Option<std::sync::Arc<str>> {
        self.shared_board.save_notices.take_pending()
    }

    /// Тест: «файл мог измениться» без потока слежения.
    #[cfg(test)]
    pub(crate) fn note_config_file_may_have_changed_for_test(&mut self) {
        self.shared_board.awaiting_turn = Some(AwaitingTurn {
            retry_not_before: None,
        });
    }

    /// Шаг доски за кадр: опрос принятия в полёте или разбор нового сигнала.
    pub(crate) fn advance_shared_settings_board<A>(
        &mut self,
        runtime_adapter: &mut A,
        now: Instant,
    ) -> SharedBoardFrame
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        if self.controller.has_pending_external_adoption() {
            return self.poll_external_adoption_with_runtime_adapter(runtime_adapter, now);
        }
        if let Some(receiver) = &self.shared_board.receiver
            && receiver.take_change() == ConfigFileChange::MayHaveChanged
        {
            // Новое событие снимает паузу повтора: смотреть нужно на свежий файл.
            self.shared_board.awaiting_turn = Some(AwaitingTurn {
                retry_not_before: None,
            });
        }
        let Some(awaiting) = self.shared_board.awaiting_turn else {
            return SharedBoardFrame::idle();
        };
        // «Чужое ждёт своего»: свой Apply запланирован или любая транзакция ждёт маршрут.
        if self.pending_apply.is_some() || self.controller.has_pending_apply() {
            return SharedBoardFrame::idle();
        }
        if awaiting
            .retry_not_before
            .is_some_and(|retry_at| now < retry_at)
        {
            return SharedBoardFrame::idle();
        }
        self.shared_board.awaiting_turn = None;
        self.adopt_settings_file(runtime_adapter, now)
    }

    /// Отменяет принятие в полёте на lifecycle-границе (suspend/выход); свой Apply не трогает.
    pub(crate) fn cancel_external_adoption<A>(&mut self, runtime_adapter: &mut A)
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        let report = self.with_apply_delegate(runtime_adapter, |controller, delegate| {
            controller.cancel_external_adoption(delegate)
        });
        let Some(report) = report else {
            return;
        };
        self.shared_board.in_flight = None;
        self.invalidate_ui_model();
        tracing::info!(
            final_state = ?report.transaction.final_state,
            "Applying settings from another Fastiplayer window was interrupted at a lifecycle boundary"
        );
        // Файл по-прежнему отличается от committed: первый кадр после возврата посмотрит
        // снова. Будильник не нужен — пока окна нет, смотреть некому.
        match &self.shared_board.receiver {
            Some(receiver) => receiver.keep_change_for_next_frame(),
            None => {
                self.shared_board.awaiting_turn = Some(AwaitingTurn {
                    retry_not_before: None,
                });
            }
        }
    }

    /// Читает файл и решает, что с ним делать.
    fn adopt_settings_file<A>(&mut self, runtime_adapter: &mut A, now: Instant) -> SharedBoardFrame
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        let fingerprint = ConfigFileFingerprint::of(&self.config_path);
        if fingerprint.is_some() && fingerprint == self.shared_board.settled_fingerprint {
            // То же содержимое уже разобрано без успеха: не зацикливаемся.
            return SharedBoardFrame::idle();
        }
        match read_shared_config_at(&self.config_path) {
            SharedConfigRead::Missing => {
                // Файл удалили: применять нечего, а появившийся заново файл — новая история.
                self.shared_board.settled_fingerprint = None;
                self.shared_board.announced_problem = None;
                SharedBoardFrame::idle()
            }
            SharedConfigRead::Unreadable { problem } => self.report_settings_file_problem(
                SettingsFileProblem::Unreadable(problem),
                fingerprint,
            ),
            SharedConfigRead::Newer { found } => {
                self.report_settings_file_problem(SettingsFileProblem::Newer { found }, fingerprint)
            }
            SharedConfigRead::Parsed(disk) => {
                self.shared_board.settled_fingerprint = None;
                self.shared_board.announced_problem = None;
                match self.write_unsaved_settings_before_adoption(*disk) {
                    Some(disk) => {
                        self.begin_adopting_disk_config(&disk, fingerprint, runtime_adapter, now)
                    }
                    // Своя дозапись изменила файл: её событие слежения придёт следом.
                    None => SharedBoardFrame::idle(),
                }
            }
        }
    }

    /// Файл нельзя принять: запомнить содержимое и сообщить один раз на новую проблему.
    fn report_settings_file_problem(
        &mut self,
        problem: SettingsFileProblem,
        fingerprint: Option<ConfigFileFingerprint>,
    ) -> SharedBoardFrame {
        self.shared_board.settled_fingerprint = fingerprint;
        if self.shared_board.announced_problem.as_ref() == Some(&problem) {
            return SharedBoardFrame::idle();
        }
        tracing::warn!(
            ?problem,
            "Settings file was changed by another program but can't be used; keeping current settings"
        );
        let notice = problem.notice();
        self.shared_board.announced_problem = Some(problem);
        SharedBoardFrame::with_notice(notice)
    }

    /// Дописывает несохранённые изменения этой копии (файл снова читается нормально).
    ///
    /// Возвращает документ, с которым сравнивать: прежний, если дописывать было нечего или
    /// не удалось, и свежий с диска после успешной дозаписи. `None` — дозапись удалась, но
    /// свежий файл прочитать не вышло: сравнивать со старым нельзя (откатили бы только что
    /// записанное), а событие своей записи всё равно придёт.
    fn write_unsaved_settings_before_adoption(&mut self, disk: AppConfig) -> Option<AppConfig> {
        if !self.store.has_unsaved_changes() {
            return Some(disk);
        }
        match self.store.save_unsaved_changes(self.controller.committed()) {
            UnsavedChangesSave::NothingToSave => Some(disk),
            UnsavedChangesSave::Saved {
                written,
                kept_from_disk,
                ..
            } => {
                self.invalidate_ui_model();
                self.shared_board.save_notices.note_saved();
                tracing::info!(
                    ?written,
                    ?kept_from_disk,
                    "Unsaved settings were written once the settings file became usable"
                );
                match read_shared_config_at(&self.config_path) {
                    SharedConfigRead::Parsed(fresh) => Some(*fresh),
                    _ => None,
                }
            }
            UnsavedChangesSave::StillUnsaved(deferral) => {
                // Та же причина, что уже показана, повторно не показывается.
                self.shared_board
                    .save_notices
                    .note_not_saved_yet(&deferral.user_message());
                tracing::info!("Unsaved settings are still waiting for the settings file");
                Some(disk)
            }
            UnsavedChangesSave::Rejected(error) => {
                tracing::warn!(%error, "Unsaved settings could not be merged into the settings file");
                Some(disk)
            }
        }
    }

    /// Сравнивает файл с committed и начинает принятие разницы.
    fn begin_adopting_disk_config<A>(
        &mut self,
        disk: &AppConfig,
        fingerprint: Option<ConfigFileFingerprint>,
        runtime_adapter: &mut A,
        now: Instant,
    ) -> SharedBoardFrame
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        // Несохранённые правки этой копии новее того, что лежит на диске.
        let unsaved_setting_ids = self.store.unsaved_setting_ids();
        let changes = match adoptable_peer_changes(
            &self.registry,
            self.controller.committed(),
            disk,
            &unsaved_setting_ids,
        ) {
            Ok(changes) => changes,
            Err(error) => {
                tracing::error!(%error, "Could not compare the settings file with current settings");
                self.shared_board.settled_fingerprint = fingerprint;
                return SharedBoardFrame::with_notice(SettingsFileNotice::AdoptionFailed);
            }
        };
        if changes.is_empty() {
            // Своя запись, повтор события или только запоминаемое состояние соседа.
            return SharedBoardFrame::idle();
        }
        let attempt = AdoptionAttempt {
            fingerprint,
            setting_ids: changes
                .iter()
                .map(|change| change.setting_id.clone())
                .collect(),
        };
        let begin = self.with_apply_delegate(runtime_adapter, |controller, delegate| {
            controller.begin_adopt_external_settings(changes, delegate)
        });
        self.invalidate_ui_model();
        match begin {
            Ok(BeginExternalAdoption::NothingToAdopt) => SharedBoardFrame::idle(),
            Ok(BeginExternalAdoption::Busy) => {
                // Проверено выше, но транзакция одна на контроллер: ждём своей очереди.
                self.shared_board.awaiting_turn = Some(AwaitingTurn {
                    retry_not_before: None,
                });
                SharedBoardFrame::idle()
            }
            Ok(BeginExternalAdoption::Pending) => {
                self.shared_board.in_flight = Some(attempt);
                SharedBoardFrame::changed()
            }
            Ok(BeginExternalAdoption::Completed(report)) => {
                self.conclude_external_adoption(*report, attempt, runtime_adapter, now)
            }
            Err(error) => {
                tracing::error!(%error, settings = ?attempt.setting_ids, "Settings from another Fastiplayer window were rejected");
                self.shared_board.settled_fingerprint = attempt.fingerprint;
                SharedBoardFrame::with_notice(SettingsFileNotice::AdoptionFailed)
            }
        }
    }

    /// Неблокирующе продвигает принятие, ждущее маршрут.
    fn poll_external_adoption_with_runtime_adapter<A>(
        &mut self,
        runtime_adapter: &mut A,
        now: Instant,
    ) -> SharedBoardFrame
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        let polled = self.with_apply_delegate(runtime_adapter, |controller, delegate| {
            controller.poll_external_adoption(delegate)
        });
        match polled {
            PollExternalAdoption::Pending => SharedBoardFrame::idle(),
            PollExternalAdoption::Idle => {
                self.shared_board.in_flight = None;
                SharedBoardFrame::idle()
            }
            PollExternalAdoption::Completed(report) => {
                let attempt = self.shared_board.in_flight.take().unwrap_or_default();
                self.invalidate_ui_model();
                self.conclude_external_adoption(*report, attempt, runtime_adapter, now)
            }
        }
    }

    /// Итог принятия для остальных слоёв.
    ///
    /// Как `conclude_apply`, но без статуса «Settings saved» и без хуков после записи:
    /// committed snapshot получают потребители, всё остальное — журнал и уведомление.
    fn conclude_external_adoption<A>(
        &mut self,
        report: ExternalAdoptionReport,
        attempt: AdoptionAttempt,
        runtime_adapter: &mut A,
        now: Instant,
    ) -> SharedBoardFrame
    where
        A: SettingsRuntimeReconfigureHost,
    {
        let final_state = report.transaction.final_state;
        match report.outcome {
            ExternalAdoptionOutcome::Adopted => {
                runtime_adapter.sync_committed_config_snapshot(
                    CommittedConfigSnapshot::from_config(self.controller.committed()),
                );
                tracing::info!(
                    settings = ?attempt.setting_ids,
                    kept_user_edits = ?report.draft_merge.kept_user_edits,
                    "Applied settings saved by another Fastiplayer window"
                );
                SharedBoardFrame::changed()
            }
            ExternalAdoptionOutcome::Refused if final_state == ApplyFinalState::RuntimeBlocked => {
                // Плеер занят перемоткой/скрабом/пересборкой — не ошибка: повторим позже.
                tracing::debug!(
                    settings = ?attempt.setting_ids,
                    "Player is busy; settings from another Fastiplayer window will be applied shortly"
                );
                self.shared_board.awaiting_turn = Some(AwaitingTurn {
                    retry_not_before: Some(now + RETRY_AFTER_RUNTIME_BUSY),
                });
                SharedBoardFrame::idle()
            }
            ExternalAdoptionOutcome::Refused | ExternalAdoptionOutcome::RolledBack => {
                tracing::warn!(
                    settings = ?attempt.setting_ids,
                    ?final_state,
                    "Settings from another Fastiplayer window could not be applied; keeping current settings"
                );
                self.shared_board.settled_fingerprint = attempt.fingerprint;
                SharedBoardFrame::with_notice(SettingsFileNotice::AdoptionFailed)
            }
            ExternalAdoptionOutcome::RollbackFailed => {
                tracing::error!(
                    settings = ?attempt.setting_ids,
                    ?final_state,
                    "Settings from another Fastiplayer window failed and the rollback failed too"
                );
                self.shared_board.settled_fingerprint = attempt.fingerprint;
                SharedBoardFrame::with_notice(SettingsFileNotice::AdoptionRollbackFailed)
            }
        }
    }

    /// Собирает делегат транзакции и даёт его контроллеру (тот же, что у Apply; шаг
    /// persist принятию недоступен на уровне типов `settings-core`).
    fn with_apply_delegate<A, R>(
        &mut self,
        runtime_adapter: &mut A,
        operation: impl FnOnce(
            &mut SettingsController<AppConfig>,
            &mut SettingsRuntimeApplyDelegate<'_, A>,
        ) -> R,
    ) -> R
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        let automatic_surface_pool = self.automatic_surface_pool();
        let mut delegate = SettingsRuntimeApplyDelegate {
            validator: AppConfigValidator,
            store: &mut self.store,
            route_appliers: &mut self.route_appliers,
            runtime_adapter,
            applied_route_count: 0,
            automatic_surface_pool,
            route_sequence: &mut self.route_sequence,
            save_notices: &mut self.shared_board.save_notices,
        };
        operation(&mut self.controller, &mut delegate)
    }
}
