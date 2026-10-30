//! С какого места стартует подготовленное медиа и кто за это место отвечает.
//!
//! Подготовленное медиа может прийти уже стоящим на позиции
//! ([`player_core::PreparedInitialPosition::PositionedAt`]): открыватель сам перемотал
//! демуксер, и плеер ставит медиа сразу туда, без второй перемотки. Путь открытия
//! `AppState` обязан понимать, ЧЬЁ это решение, иначе он не может проверить
//! доказательство плеера:
//! - обычный плеер (очередь, старт, Undo Clear, native HLS): позицию решает
//!   приложение ([`StartupPosition`]), и готовая позиция допустима только как
//!   доказательство ТОЙ ЖЕ цели восстановления — иначе отказ (защита от медиа,
//!   поставленного «не туда», см. [`super::prepared_position_restore_strategy`]);
//! - поколение браузерного потока (мост, 03e/04a): позицию решил владелец источника
//!   — страница назвала её, и `SessionMedia` перемотал поток до открытия. Цели
//!   восстановления у такого медиа нет и быть не может, а сама позиция авторитетна:
//!   путь открытия принимает доказательство плеера как есть ([`PreparedBySource`]).
//!
//! Разделение явное и типизированное, чтобы ослабление не просочилось в обычный
//! плеер: `StartupPosition::KeepStart` с готовой позицией по-прежнему отказ.
//!
//! [`PreparedBySource`]: PreparedOpenStartPosition::PreparedBySource

use std::time::Duration;

use player_core::{PreparedInitialPosition, PreparedMedia};

use super::pending::{
    PostInstalledStartupPositionFacts, PostInstalledStartupPositionPlan,
    plan_post_installed_startup_position,
};
use super::{
    PreparedPositionRestoreContractError, PreparedPositionRestoreStrategy, PreparedSingleMediaOpen,
    prepared_position_restore_strategy,
};
use crate::media_open::{ActiveMediaSource, SafeMediaLabel};
use crate::playlist_runtime::StartupPosition;

/// Кто решает, с какого места стартует подготовленное медиа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedOpenStartPosition {
    /// Решает приложение: начало или восстановление позиции очереди. Готовая позиция
    /// медиа обязана совпасть с целью восстановления.
    Startup(StartupPosition),
    /// Решил владелец источника, подготовив медиа (поколение браузерного потока):
    /// с начала — так с начала, стоит на позиции — значит, с неё. Проверять её не
    /// с чем, путь открытия принимает доказательство плеера без второй перемотки.
    PreparedBySource,
}

/// Что сделать с позицией после установки — решено при приёме открытия, до установки.
///
/// Владелец — ждущая транзакция открытия: план не меняется до `Installed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingStartPositionPlan {
    /// Позицию решает приложение; стратегия выведена из доказательства плеера.
    Startup {
        position: StartupPosition,
        strategy: PreparedPositionRestoreStrategy,
    },
    /// Владелец источника поставил медиа на `target`: после установки — только
    /// усыновить готовую позицию плеера (сверка цели, без перемотки демуксера).
    AdoptSourcePrepared { target: Duration },
}

impl PendingStartPositionPlan {
    /// План пути, где позицию ищет обычная перемотка после установки (очередь,
    /// переоткрытие той же линии): готовой позиции у такого медиа не бывает.
    pub(crate) const fn seek_after_install(position: StartupPosition) -> Self {
        Self::Startup {
            position,
            strategy: PreparedPositionRestoreStrategy::SeekAfterInstall,
        }
    }

    /// Принимает открытие: сверяет доказательство плеера с тем, кто решает позицию.
    ///
    /// # Errors
    /// Готовая позиция без цели восстановления или с другой целью, когда позицию
    /// решает приложение (отказ, как и до появления браузерных поколений).
    pub(crate) fn admit(
        prepared_initial_position: PreparedInitialPosition,
        start: PreparedOpenStartPosition,
    ) -> Result<Self, PreparedPositionRestoreContractError> {
        match start {
            PreparedOpenStartPosition::Startup(position) => Ok(Self::Startup {
                position,
                strategy: prepared_position_restore_strategy(prepared_initial_position, position)?,
            }),
            PreparedOpenStartPosition::PreparedBySource => Ok(match prepared_initial_position {
                // Источник начинает с начала: делать после установки нечего.
                PreparedInitialPosition::Beginning => {
                    Self::seek_after_install(StartupPosition::KeepStart)
                }
                PreparedInitialPosition::PositionedAt {
                    target_position, ..
                } => Self::AdoptSourcePrepared {
                    target: target_position.as_duration(),
                },
            }),
        }
    }

    /// Шаг после установки по фактам плеера (длительность, вид шкалы, позиция).
    pub(crate) fn post_installed_plan(
        self,
        facts: PostInstalledStartupPositionFacts,
    ) -> PostInstalledStartupPositionPlan {
        match self {
            Self::Startup { position, strategy } => {
                plan_post_installed_startup_position(position, strategy, facts)
            }
            // Ни политики «у конца — с начала», ни предупреждений о позиции дальше
            // длительности: это не возобновление просмотра, а место, которое страница
            // уже показывает. Плеер сверяет цель и не перематывает второй раз.
            Self::AdoptSourcePrepared { target } => PostInstalledStartupPositionPlan::Restore {
                requested_position: target,
                position: player_core::InstalledPositionRestore::AdoptPreparedInitialPosition {
                    expected_target: target,
                },
            },
        }
    }
}

impl PreparedSingleMediaOpen {
    /// Медиа, чьё место старта решил владелец источника (поколение браузерного
    /// потока): готовая позиция, если она есть, принимается как авторитетная.
    pub(crate) fn prepared_by_source(
        prepared_media: PreparedMedia,
        source: ActiveMediaSource,
        safe_label: SafeMediaLabel,
    ) -> Self {
        let mut open = Self::new(prepared_media, source, safe_label);
        open.start_position = PreparedOpenStartPosition::PreparedBySource;
        open
    }

    /// План позиции для ждущей транзакции; вызывается при приёме открытия.
    pub(super) fn admit_start_position(
        &self,
    ) -> Result<PendingStartPositionPlan, PreparedPositionRestoreContractError> {
        PendingStartPositionPlan::admit(
            self.prepared_media.prepared_initial_position(),
            self.start_position,
        )
    }
}

#[cfg(test)]
#[path = "start_position_tests.rs"]
mod tests;
