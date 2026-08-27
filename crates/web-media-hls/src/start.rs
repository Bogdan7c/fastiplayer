//! Типизированная политика initial HLS VOD open и её наблюдаемый результат.

use std::time::Duration;

use media_core::MediaTime;

/// Явно задаёт пользовательскую точку initial HLS VOD open без позднего seek от начала.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlsVodStartIntent {
    /// Обычный open с первого media segment-а.
    Beginning,
    /// Строгий restore: позиция за границей VOD остаётся terminal ошибкой.
    Restore(MediaTime),
    /// Restore пользовательского checkpoint-а с честным fallback на начало finite VOD.
    RestoreOrBeginning(MediaTime),
}

/// Причина, по которой неавторитетный checkpoint не стал initial restore-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlsVodRestoreFallbackReason {
    /// Сохранённая позиция находится за manifest-derived finite VOD duration.
    CheckpointOutsideVod,
}

/// Наблюдаемый итог применения caller-owned initial start policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlsVodStartDisposition {
    /// Caller явно запросил начало VOD.
    BeginningRequested,
    /// Restore target принят manifest plan-ом; actual RAP proof остаётся deferred.
    RestoreRequested {
        /// Exact сохранённая позиция, которую должен доказать initial demux open.
        target_position: MediaTime,
    },
    /// Restore отклонён до media I/O, а тот же parsed plan открыт с начала.
    RestoreRejectedToBeginning {
        /// Отклонённая сохранённая позиция.
        target_position: MediaTime,
        /// Типизированная причина fallback-а без ложного seek receipt/proof.
        reason: HlsVodRestoreFallbackReason,
    },
}

impl HlsVodStartDisposition {
    /// Даёт строгий effective intent для последующих components того же selection-а.
    pub(crate) const fn effective_start(self) -> HlsResolvedVodStartIntent {
        match self {
            Self::BeginningRequested | Self::RestoreRejectedToBeginning { .. } => {
                HlsResolvedVodStartIntent::Beginning
            }
            Self::RestoreRequested { target_position } => {
                HlsResolvedVodStartIntent::Restore(target_position)
            }
        }
    }

    /// Не позволяет alternate component повторно применять permissive fallback независимо от main.
    pub(crate) const fn strict_component_start(self) -> HlsVodStartIntent {
        match self.effective_start() {
            HlsResolvedVodStartIntent::Beginning => HlsVodStartIntent::Beginning,
            HlsResolvedVodStartIntent::Restore(target_position) => {
                HlsVodStartIntent::Restore(target_position)
            }
        }
    }
}

/// Нормализованный owner-private intent: permissive policy сюда уже не просачивается.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HlsResolvedVodStartIntent {
    Beginning,
    Restore(MediaTime),
}

/// Manifest-derived effective start вместе с обязательным public disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HlsResolvedVodStart {
    pub(crate) intent: HlsResolvedVodStartIntent,
    pub(crate) disposition: HlsVodStartDisposition,
}

/// Строгая policy сохраняет прежнюю terminal semantics для позиции за duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HlsStrictRestoreOutsideVod;

impl HlsVodStartIntent {
    /// Один раз применяет caller policy к уже доказанной finite duration до media GET.
    pub(crate) fn resolve_for_duration(
        self,
        duration: Duration,
    ) -> Result<HlsResolvedVodStart, HlsStrictRestoreOutsideVod> {
        match self {
            Self::Beginning => Ok(HlsResolvedVodStart {
                intent: HlsResolvedVodStartIntent::Beginning,
                disposition: HlsVodStartDisposition::BeginningRequested,
            }),
            Self::Restore(target_position) => {
                if target_position.as_duration() > duration {
                    Err(HlsStrictRestoreOutsideVod)
                } else {
                    Ok(accepted_restore(target_position))
                }
            }
            Self::RestoreOrBeginning(target_position) => {
                if target_position.as_duration() > duration {
                    Ok(HlsResolvedVodStart {
                        intent: HlsResolvedVodStartIntent::Beginning,
                        disposition: HlsVodStartDisposition::RestoreRejectedToBeginning {
                            target_position,
                            reason: HlsVodRestoreFallbackReason::CheckpointOutsideVod,
                        },
                    })
                } else {
                    Ok(accepted_restore(target_position))
                }
            }
        }
    }
}

fn accepted_restore(target_position: MediaTime) -> HlsResolvedVodStart {
    HlsResolvedVodStart {
        intent: HlsResolvedVodStartIntent::Restore(target_position),
        disposition: HlsVodStartDisposition::RestoreRequested { target_position },
    }
}
