//! Дешёвый lookup идентичности frame resource-а без дублирования platform handles.
//!
//! Рендерер держит кэш импортированных image по стабильной идентичности буфера
//! (`video_core::FrameResourceIdentity`). Чтобы на попадании в кэш не делать
//! `dup()`/`close()` fd и не трогать descriptor вовсе, provider отдаёт сначала
//! только идентичность, а полноценный descriptor с fd запрашивается лишь при
//! промахе. Результат сохраняет те же различимые состояния, что и descriptor
//! lookup (`Busy`/`Missing`/`Fatal`), плюс явное «provider этого не умеет».

use std::time::Duration;

/// Результат identity lookup-а для renderer-side кэша импортов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentFrameResourceIdentityLookup {
    /// Provider знает стабильную идентичность ресурса под этим handle-ом.
    Ready {
        /// Идентичность буфера без platform fd.
        identity: video_core::FrameResourceIdentity,

        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        resource_pool_lock_wait: Duration,
    },

    /// Backend resource pool занят, render hot path должен выбрать fallback без ожидания.
    Busy {
        /// Сколько заняла non-blocking попытка получить lock.
        resource_pool_lock_wait: Duration,
    },

    /// Backend доступен, но resource для handle отсутствует.
    Missing {
        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        resource_pool_lock_wait: Duration,
    },

    /// Backend обнаружил poisoned/fatal state при lookup-е.
    Fatal {
        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        resource_pool_lock_wait: Duration,
    },

    /// Provider не умеет отдавать идентичность (например, host-planar backend).
    ///
    /// Это не ошибка: caller обязан перейти к обычному descriptor lookup-у.
    Unsupported,
}

impl PresentFrameResourceIdentityLookup {
    /// Возвращает lock wait sample; для `Unsupported` ожидания не было.
    #[must_use]
    pub const fn resource_pool_lock_wait(&self) -> Duration {
        match self {
            Self::Ready {
                resource_pool_lock_wait,
                ..
            }
            | Self::Busy {
                resource_pool_lock_wait,
            }
            | Self::Missing {
                resource_pool_lock_wait,
            }
            | Self::Fatal {
                resource_pool_lock_wait,
            } => *resource_pool_lock_wait,
            Self::Unsupported => Duration::ZERO,
        }
    }
}
