//! Очередь команд worker-а с упорядоченным резервом на случай переполнения.
//!
//! Зачем: UI шлёт команды неблокирующе. Если поток worker-а надолго занят (открытие
//! аудиоустройства, синхронный seek локального файла, flush декодера), bounded очередь
//! на `COMMAND_CHANNEL_CAPACITY` команд заполняется, и раньше «терминальная» команда
//! (отпустил ползунок, финальная громкость, play/pause) просто терялась. Плеер мог
//! навсегда остаться в `Scrubbing` (сессия UX 17).
//!
//! Устройство:
//! - основная очередь — прежний crossbeam-канал, быстрый путь без блокировок;
//! - резерв — небольшая упорядоченная очередь за ней. Команда, не влезшая в основную
//!   очередь, ложится в резерв, а не теряется;
//! - промежуточные «последние значения» (`PlayerCommandDelivery::LatestValue`) в резерве
//!   сливаются с соседней командой того же вида, поэтому drag громкости во время
//!   зависания занимает одну ячейку, а не сотни.
//!
//! Инвариант порядка (главное, что охраняет этот модуль):
//! 1. Пока резерв не пуст, обычные команды идут только в резерв — даже если в основной
//!    очереди освободилось место. Иначе более новая команда обогнала бы старую.
//! 2. Команды с receipt (`try_send_ordered_worker_command`) при непустом резерве получают
//!    `Full`, то есть привычный caller-у backpressure, а не обгон пользовательских намерений.
//! 3. Worker берёт из резерва только когда основная очередь пуста: всё, что в ней лежит,
//!    было отправлено раньше любой команды резерва.
//!
//! Единственное сознательное исключение — terminal shutdown (`try_send_bypassing_reserve`):
//! он обязан обгонять всё.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded};
use tracing::{info, warn};

use super::{PlayerWorkerSendError, WorkerCommand};
use crate::PlayerCommand;

/// Сколько обычных команд резерв держит сверх основной очереди.
///
/// Благодаря слиянию latest-value команд сюда попадают в основном ordered-команды
/// (End/Cancel scrub, toggle, seek с клавиатуры). 64 таких действия за время одного
/// зависания worker-а — уже далеко за пределами реального пользовательского ввода.
const COMMAND_RESERVE_CAPACITY: usize = 64;

/// Как команду разрешено доставлять при backpressure.
///
/// Классификация — часть контракта доставки: новая `PlayerCommand` обязана явно выбрать
/// класс (match ниже без wildcard-а), иначе код не скомпилируется.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlayerCommandDelivery {
    /// Абсолютное значение: следующая команда того же вида полностью заменяет предыдущую.
    LatestValue(LatestValueCommandKind),

    /// Каждая команда значима сама по себе и в своём порядке; никогда не сливается.
    Ordered,
}

/// Вид latest-value команды: сливаются только команды одного вида.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LatestValueCommandKind {
    /// `SetVolume`: важна только последняя громкость.
    Volume,

    /// `SetPlaybackRate`: абсолютная скорость, важна последняя.
    PlaybackRate,

    /// `UpdateScrub` / `PreviewScrub`: latest-only цель внутри уже начатого scrub-а.
    ScrubTarget,
}

impl PlayerCommandDelivery {
    /// Классифицирует player command для резерва.
    pub(super) const fn of(command: &PlayerCommand) -> Self {
        match command {
            PlayerCommand::SetVolume(_) => Self::LatestValue(LatestValueCommandKind::Volume),
            PlayerCommand::SetPlaybackRate(_) => {
                Self::LatestValue(LatestValueCommandKind::PlaybackRate)
            }
            PlayerCommand::UpdateScrub(_) | PlayerCommand::PreviewScrub { .. } => {
                Self::LatestValue(LatestValueCommandKind::ScrubTarget)
            }
            // Begin/End scrub ограничивают жест, toggle-команды относительные (двойной
            // toggle ≠ одиночный), а seek/open/track — самостоятельные намерения.
            PlayerCommand::OpenMedia(_)
            | PlayerCommand::Play
            | PlayerCommand::Pause
            | PlayerCommand::TogglePlayback
            | PlayerCommand::Seek(_)
            | PlayerCommand::BeginScrub { .. }
            | PlayerCommand::EndScrub { .. }
            | PlayerCommand::Stop
            | PlayerCommand::ToggleMute { .. }
            | PlayerCommand::SelectVideoTrack(_)
            | PlayerCommand::SelectAudioTrack(_)
            | PlayerCommand::SelectSubtitleTrack(_)
            | PlayerCommand::SelectQuality(_)
            | PlayerCommand::ReloadConfig
            | PlayerCommand::Shutdown => Self::Ordered,
        }
    }

    /// Класс доставки worker command: всё, кроме `Player`, — ordered.
    const fn of_worker_command(command: &WorkerCommand) -> Self {
        match command {
            WorkerCommand::Player(player_command) => Self::of(player_command),
            _ => Self::Ordered,
        }
    }
}

/// Как обычная команда была принята очередью (для тестов и диагностики).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandAdmission {
    /// Легла в основную очередь — обычный быстрый путь.
    Queued,

    /// Основная очередь занята или резерв уже используется: команда в резерве.
    Reserved,

    /// Заменила соседнюю latest-value команду того же вида в резерве.
    CoalescedInReserve,
}

/// Счётчики одного эпизода переполнения: от первой команды в резерве до его опустошения.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct CommandReserveEpisode {
    /// Команды, положенные в резерв отдельной ячейкой.
    pub(super) reserved: u64,

    /// Latest-value команды, слитые с соседней командой резерва.
    pub(super) coalesced: u64,

    /// Команды, отвергнутые с `Full`: резерв был полон (это и есть реальные потери).
    pub(super) rejected: u64,
}

impl CommandReserveEpisode {
    const fn has_activity(self) -> bool {
        self.reserved > 0 || self.coalesced > 0 || self.rejected > 0
    }
}

/// Состояние резерва под одним mutex-ом: и решение «куда класть», и сами команды.
#[derive(Default)]
struct CommandReserveState {
    /// Упорядоченные команды, ждущие опустошения основной очереди.
    commands: VecDeque<WorkerCommand>,

    /// Счётчики текущего эпизода переполнения.
    episode: CommandReserveEpisode,

    /// Worker-сторона закрыта: резерв больше не может принять команду «на будущее».
    inbox_closed: bool,
}

/// Общий для sender-ов и worker-а резерв.
struct CommandReserve {
    state: Mutex<CommandReserveState>,

    /// Capacity-one wake: будит worker, если резерв пополнился, пока тот спал.
    wake_tx: Sender<()>,
}

impl CommandReserve {
    /// Берёт state даже после panic-а другого держателя: state — простые данные без
    /// промежуточных инвариантов, потерять из-за poison пользовательскую команду хуже.
    fn lock_state(&self) -> MutexGuard<'_, CommandReserveState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Будит worker; заполненный wake-канал означает, что пробуждение уже запланировано.
    fn wake_worker(&self) {
        let _already_pending_or_closed = self.wake_tx.try_send(());
    }
}

/// Отправляющая сторона очереди команд (живёт внутри `PlayerCommandSender`).
#[derive(Clone)]
pub(super) struct WorkerCommandQueue {
    main_tx: Sender<WorkerCommand>,
    reserve: Arc<CommandReserve>,
}

/// Принимающая сторона очереди команд (живёт внутри `PlayerWorkerRuntime`).
pub(super) struct WorkerCommandInbox {
    main_rx: Receiver<WorkerCommand>,
    reserve: Arc<CommandReserve>,
    reserve_wake_rx: Receiver<()>,
}

/// Собирает очередь вокруг уже созданного bounded канала.
pub(super) fn worker_command_queue(
    main_tx: Sender<WorkerCommand>,
    main_rx: Receiver<WorkerCommand>,
) -> (WorkerCommandQueue, WorkerCommandInbox) {
    let (wake_tx, reserve_wake_rx) = bounded(1);
    let reserve = Arc::new(CommandReserve {
        state: Mutex::new(CommandReserveState::default()),
        wake_tx,
    });
    (
        WorkerCommandQueue {
            main_tx,
            reserve: Arc::clone(&reserve),
        },
        WorkerCommandInbox {
            main_rx,
            reserve,
            reserve_wake_rx,
        },
    )
}

impl WorkerCommandQueue {
    /// Обычная player command: не блокирует и не теряется, пока резерв не полон.
    pub(super) fn try_send_player_command(
        &self,
        command: PlayerCommand,
    ) -> Result<CommandAdmission, PlayerWorkerSendError> {
        let mut state = self.reserve.lock_state();
        if state.inbox_closed {
            return Err(PlayerWorkerSendError::Disconnected);
        }

        let command = WorkerCommand::Player(command);
        let command = if state.commands.is_empty() {
            match self.main_tx.try_send(command) {
                Ok(()) => return Ok(CommandAdmission::Queued),
                Err(TrySendError::Disconnected(_command)) => {
                    return Err(PlayerWorkerSendError::Disconnected);
                }
                Err(TrySendError::Full(command)) => {
                    warn!(
                        "Очередь команд player worker-а заполнена: команды временно уходят в резерв"
                    );
                    command
                }
            }
        } else {
            command
        };

        let admission = Self::push_into_reserve(&mut state, command, ReserveLimit::Bounded)?;
        drop(state);
        self.reserve.wake_worker();
        Ok(admission)
    }

    /// Команда с receipt: при непустом резерве — `Full`, чтобы не обогнать пользователя.
    pub(super) fn try_send_ordered_worker_command(
        &self,
        command: WorkerCommand,
    ) -> Result<(), PlayerWorkerSendError> {
        let state = self.reserve.lock_state();
        if state.inbox_closed {
            return Err(PlayerWorkerSendError::Disconnected);
        }
        if !state.commands.is_empty() {
            return Err(PlayerWorkerSendError::Full);
        }
        // Mutex держится и во время try_send: иначе между проверкой и отправкой другой
        // sender мог бы положить команду в резерв, и эта команда её обогнала бы.
        self.main_tx
            .try_send(command)
            .map_err(PlayerWorkerSendError::from)
    }

    /// Cleanup-команда без потери: при переполнении ложится в резерв сверх лимита.
    ///
    /// Раньше этот путь блокировал caller до появления места в основной очереди; теперь
    /// команда сохраняется в том же порядке без блокировки UI-потока.
    pub(super) fn send_lossless_worker_command(
        &self,
        command: WorkerCommand,
    ) -> Result<(), PlayerWorkerSendError> {
        let mut state = self.reserve.lock_state();
        if state.inbox_closed {
            return Err(PlayerWorkerSendError::Disconnected);
        }
        let command = if state.commands.is_empty() {
            match self.main_tx.try_send(command) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(_command)) => {
                    return Err(PlayerWorkerSendError::Disconnected);
                }
                Err(TrySendError::Full(command)) => command,
            }
        } else {
            command
        };
        Self::push_into_reserve(&mut state, command, ReserveLimit::Unbounded)?;
        drop(state);
        self.reserve.wake_worker();
        Ok(())
    }

    /// Очередь без worker-стороны резерва: для fixture-ов, читающих сырой receiver.
    #[cfg(test)]
    pub(super) fn detached_for_tests(main_tx: Sender<WorkerCommand>) -> Self {
        let (wake_tx, _reserve_wake_rx) = bounded(1);
        Self {
            main_tx,
            reserve: Arc::new(CommandReserve {
                state: Mutex::new(CommandReserveState::default()),
                wake_tx,
            }),
        }
    }

    /// Terminal shutdown: сознательно обгоняет резерв (см. модульную документацию).
    pub(super) fn try_send_bypassing_reserve(
        &self,
        command: WorkerCommand,
    ) -> Result<(), PlayerWorkerSendError> {
        self.main_tx
            .try_send(command)
            .map_err(PlayerWorkerSendError::from)
    }

    /// Кладёт команду в хвост резерва, сливая соседние latest-value команды одного вида.
    fn push_into_reserve(
        state: &mut CommandReserveState,
        command: WorkerCommand,
        limit: ReserveLimit,
    ) -> Result<CommandAdmission, PlayerWorkerSendError> {
        let delivery = PlayerCommandDelivery::of_worker_command(&command);
        if let PlayerCommandDelivery::LatestValue(kind) = delivery
            && let Some(tail) = state.commands.back_mut()
            && PlayerCommandDelivery::of_worker_command(tail)
                == PlayerCommandDelivery::LatestValue(kind)
        {
            // Сливаем только с самым последним элементом: между ними нет других команд,
            // значит замена не меняет смысл последовательности.
            *tail = command;
            state.episode.coalesced += 1;
            return Ok(CommandAdmission::CoalescedInReserve);
        }

        if limit == ReserveLimit::Bounded && state.commands.len() >= COMMAND_RESERVE_CAPACITY {
            state.episode.rejected += 1;
            warn!(
                reserve_capacity = COMMAND_RESERVE_CAPACITY,
                "Резерв команд player worker-а полон: команда отвергнута"
            );
            return Err(PlayerWorkerSendError::Full);
        }

        state.commands.push_back(command);
        state.episode.reserved += 1;
        Ok(CommandAdmission::Reserved)
    }
}

/// Лимит резерва для конкретного пути отправки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReserveLimit {
    /// Обычные команды: при полном резерве — `Full`.
    Bounded,

    /// Lossless cleanup: принимается всегда (таких команд единицы на install).
    Unbounded,
}

/// Все sender-ы закрыты, а основная очередь и резерв пусты: команд больше не будет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CommandInboxDisconnected;

impl WorkerCommandInbox {
    /// Забирает следующую команду: сначала основная очередь, затем резерв.
    ///
    /// `Ok(None)` — команд сейчас нет.
    pub(super) fn try_receive(&self) -> Result<Option<WorkerCommand>, CommandInboxDisconnected> {
        match self.main_rx.try_recv() {
            Ok(command) => return Ok(Some(command)),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }

        let mut state = self.reserve.lock_state();
        if let Some(command) = state.commands.pop_front() {
            if state.commands.is_empty() {
                Self::finish_reserve_episode(&mut state);
            }
            return Ok(Some(command));
        }
        drop(state);

        // Основная очередь могла получить команду, пока мы смотрели резерв.
        match self.main_rx.try_recv() {
            Ok(command) => Ok(Some(command)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(CommandInboxDisconnected),
        }
    }

    /// Пишет итог эпизода переполнения и сбрасывает счётчики.
    fn finish_reserve_episode(state: &mut CommandReserveState) {
        let episode = std::mem::take(&mut state.episode);
        if episode.has_activity() {
            info!(
                reserved = episode.reserved,
                coalesced = episode.coalesced,
                rejected = episode.rejected,
                "Резерв команд player worker-а разобран"
            );
        }
    }

    /// Основной канал для `select!` worker-а.
    pub(super) const fn main_receiver(&self) -> &Receiver<WorkerCommand> {
        &self.main_rx
    }

    /// Wake резерва для `select!` worker-а.
    pub(super) const fn reserve_wake_receiver(&self) -> &Receiver<()> {
        &self.reserve_wake_rx
    }

    /// Снимок счётчиков текущего эпизода переполнения.
    #[cfg(test)]
    pub(super) fn current_reserve_episode(&self) -> CommandReserveEpisode {
        self.reserve.lock_state().episode
    }
}

impl Drop for WorkerCommandInbox {
    /// После остановки worker-а резерв не должен «принимать» команды, которых никто не прочтёт.
    fn drop(&mut self) {
        let mut state = self.reserve.lock_state();
        state.inbox_closed = true;
        state.commands.clear();
    }
}

#[cfg(test)]
#[path = "command_queue/tests.rs"]
mod tests;
