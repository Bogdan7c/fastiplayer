//! Validated redirect/origin policy поверх `source-core` HTTP values.

use source_core::{HttpRequestTarget, HttpScheme};

/// Жёсткий API budget на redirect chain.
const MAX_REDIRECT_HOPS: u8 = 20;

/// Проверенный maximum redirect hops без magic integer в provider API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RedirectHopLimit(u8);

impl RedirectHopLimit {
    /// Проверяет hard safety cap.
    pub const fn new(value: u8) -> Result<Self, RedirectHopLimitError> {
        if value > MAX_REDIRECT_HOPS {
            return Err(RedirectHopLimitError::TooLarge);
        }
        Ok(Self(value))
    }

    /// Полностью запрещает redirect hops.
    #[must_use]
    pub const fn none() -> Self {
        Self(0)
    }

    /// Возвращает checked limit только для exact comparison.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }
}

/// Фактическое либо уже completed число redirect hops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RedirectHopCount(u8);

impl RedirectHopCount {
    /// Создаёт provider-observed hop count.
    #[must_use]
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Обозначает отсутствие redirects без positional `0`.
    #[must_use]
    pub const fn none() -> Self {
        Self(0)
    }

    /// Возвращает count только для policy comparison.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }
}

/// Политика перехода между security origins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectOriginPolicy {
    /// Redirect разрешён только внутри exact normalized origin.
    SameOriginOnly,
    /// Cross-origin hop разрешён, но никогда не авторизует forwarding secrets.
    CrossOriginWithoutSecrets,
}

/// Политика HTTPS → HTTP downgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecureRedirectPolicy {
    /// Downgrade полностью запрещён.
    DenyDowngrade,
    /// Downgrade допустим только как request без scoped secrets.
    AllowDowngradeWithoutSecrets,
}

/// Immutable bounded redirect policy, передаваемая concrete provider-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedirectPolicy {
    /// Максимальное число completed redirect hops.
    max_hops: RedirectHopLimit,
    /// Origin transition policy.
    origin: RedirectOriginPolicy,
    /// Secure downgrade policy.
    secure: SecureRedirectPolicy,
}

impl RedirectPolicy {
    /// Создаёт explicit policy без positional bool-ов.
    pub const fn new(
        max_hops: RedirectHopLimit,
        origin: RedirectOriginPolicy,
        secure: SecureRedirectPolicy,
    ) -> Self {
        Self {
            max_hops,
            origin,
            secure,
        }
    }

    /// Безопасный default для authenticated component requests.
    pub const fn same_origin(max_hops: RedirectHopLimit) -> Self {
        Self::new(
            max_hops,
            RedirectOriginPolicy::SameOriginOnly,
            SecureRedirectPolicy::DenyDowngrade,
        )
    }

    /// Разрешает CDN redirect, но требует безусловно снять scoped secrets.
    pub const fn cross_origin_without_secrets(max_hops: RedirectHopLimit) -> Self {
        Self::new(
            max_hops,
            RedirectOriginPolicy::CrossOriginWithoutSecrets,
            SecureRedirectPolicy::DenyDowngrade,
        )
    }

    /// Возвращает redirect hop budget.
    #[must_use]
    pub const fn max_hops(self) -> RedirectHopLimit {
        self.max_hops
    }

    /// Проверяет очередной hop и возвращает только forwarding authorization.
    pub fn authorize_redirect(
        self,
        from: &HttpRequestTarget,
        to: &HttpRequestTarget,
        completed_hops: RedirectHopCount,
    ) -> Result<RedirectAuthorization, RedirectPolicyError> {
        if completed_hops.value() >= self.max_hops.value() {
            return Err(RedirectPolicyError::HopLimitExceeded);
        }
        let same_origin = from.origin() == to.origin();
        if !same_origin && self.origin == RedirectOriginPolicy::SameOriginOnly {
            return Err(RedirectPolicyError::CrossOriginRejected);
        }
        let secure_downgrade =
            from.scheme() == HttpScheme::Https && to.scheme() == HttpScheme::Http;
        if secure_downgrade && self.secure == SecureRedirectPolicy::DenyDowngrade {
            return Err(RedirectPolicyError::SecureDowngradeRejected);
        }
        Ok(RedirectAuthorization {
            secret_forwarding_candidate: same_origin && !secure_downgrade,
        })
    }
}

/// Результат redirect validation без права самостоятельно раскрывать secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedirectAuthorization {
    /// `true` означает лишь возможность последующей проверки SecretRequestScope.
    secret_forwarding_candidate: bool,
}

impl RedirectAuthorization {
    /// Разрешает вызвать scoped secret check для target-а.
    #[must_use]
    pub const fn permits_secret_scope_check(self) -> bool {
        self.secret_forwarding_candidate
    }
}

/// Ошибка построения redirect hop limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RedirectHopLimitError {
    /// Hop limit превышает hard safety cap.
    #[error("redirect hop limit превышает допустимый budget")]
    TooLarge,
}

/// Typed redirect rejection без URL payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RedirectPolicyError {
    /// Chain исчерпал разрешённое число переходов.
    #[error("redirect chain превысил hop limit")]
    HopLimitExceeded,
    /// Policy запрещает смену security origin.
    #[error("cross-origin redirect запрещён policy")]
    CrossOriginRejected,
    /// Policy запрещает HTTPS → HTTP downgrade.
    #[error("secure redirect downgrade запрещён policy")]
    SecureDowngradeRejected,
}
