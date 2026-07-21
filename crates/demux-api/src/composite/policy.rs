//! Typed и жёстко ограниченная component lead/read-ahead policy.

use std::num::NonZeroUsize;
use std::time::Duration;

/// Максимальный timestamp lead, который S21R сможет удерживать без player progress.
const MAX_COMPONENT_TIMESTAMP_LEAD: Duration = Duration::from_secs(10 * 60);

/// Максимум bootstrap packets до появления сравнимых timestamps.
const MAX_BOOTSTRAP_PACKET_LIMIT: usize = 64;

/// Максимум bootstrap payload bytes между двумя component-ами.
const MAX_BOOTSTRAP_BYTE_LIMIT: usize = 64 * 1024 * 1024;

/// Ошибка настройки bounded component lead/read-ahead policy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompositeComponentLeadPolicyError {
    /// Нулевой lead превращает любую разницу timestamps в невозможный progress.
    #[error("composite timestamp lead должен быть больше нуля")]
    ZeroTimestampLead,
    /// Слишком большой lead разрушает bounded pending-state invariant.
    #[error("composite timestamp lead {requested:?} превышает максимум {maximum:?}")]
    TimestampLeadTooLarge {
        /// Запрошенный caller-ом lead.
        requested: Duration,
        /// Compile-time safety ceiling.
        maximum: Duration,
    },
    /// Packet cap превышает safety ceiling.
    #[error("composite bootstrap packet limit {requested} превышает максимум {maximum}")]
    PacketLimitTooLarge {
        /// Запрошенный packet count.
        requested: usize,
        /// Compile-time safety ceiling.
        maximum: usize,
    },
    /// Byte cap превышает safety ceiling.
    #[error("composite bootstrap byte limit {requested} превышает максимум {maximum}")]
    ByteLimitTooLarge {
        /// Запрошенный payload byte count.
        requested: usize,
        /// Compile-time safety ceiling.
        maximum: usize,
    },
}

/// Typed bounded policy для текущего one-slot и будущего temporary-readiness interleave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositeComponentLeadPolicy {
    /// Максимальная допустимая разница presentation timestamps готовых component-ов.
    max_timestamp_lead: Duration,
    /// Максимум packets до появления сравнимых timestamps.
    bootstrap_packet_limit: NonZeroUsize,
    /// Максимум payload bytes bootstrap pending-state.
    bootstrap_byte_limit: NonZeroUsize,
}

impl CompositeComponentLeadPolicy {
    /// Создаёт policy после проверки всех safety ceilings.
    pub fn new(
        max_timestamp_lead: Duration,
        bootstrap_packet_limit: NonZeroUsize,
        bootstrap_byte_limit: NonZeroUsize,
    ) -> Result<Self, CompositeComponentLeadPolicyError> {
        if max_timestamp_lead.is_zero() {
            return Err(CompositeComponentLeadPolicyError::ZeroTimestampLead);
        }
        if max_timestamp_lead > MAX_COMPONENT_TIMESTAMP_LEAD {
            return Err(CompositeComponentLeadPolicyError::TimestampLeadTooLarge {
                requested: max_timestamp_lead,
                maximum: MAX_COMPONENT_TIMESTAMP_LEAD,
            });
        }
        if bootstrap_packet_limit.get() > MAX_BOOTSTRAP_PACKET_LIMIT {
            return Err(CompositeComponentLeadPolicyError::PacketLimitTooLarge {
                requested: bootstrap_packet_limit.get(),
                maximum: MAX_BOOTSTRAP_PACKET_LIMIT,
            });
        }
        if bootstrap_byte_limit.get() > MAX_BOOTSTRAP_BYTE_LIMIT {
            return Err(CompositeComponentLeadPolicyError::ByteLimitTooLarge {
                requested: bootstrap_byte_limit.get(),
                maximum: MAX_BOOTSTRAP_BYTE_LIMIT,
            });
        }
        Ok(Self {
            max_timestamp_lead,
            bootstrap_packet_limit,
            bootstrap_byte_limit,
        })
    }

    /// Создаёт S21-compatible policy с одним pending packet на component.
    pub fn single_pending_packet(
        max_timestamp_lead: Duration,
        bootstrap_byte_limit: NonZeroUsize,
    ) -> Result<Self, CompositeComponentLeadPolicyError> {
        Self::new(max_timestamp_lead, NonZeroUsize::MIN, bootstrap_byte_limit)
    }

    /// Максимальный timestamp lead для S21R readiness gating.
    #[must_use]
    pub const fn max_timestamp_lead(self) -> Duration {
        self.max_timestamp_lead
    }

    /// Максимум bootstrap packets.
    #[must_use]
    pub const fn bootstrap_packet_limit(self) -> usize {
        self.bootstrap_packet_limit.get()
    }

    /// Максимум bootstrap bytes.
    #[must_use]
    pub const fn bootstrap_byte_limit(self) -> usize {
        self.bootstrap_byte_limit.get()
    }
}
