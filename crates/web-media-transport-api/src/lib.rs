//! Нейтральная граница между web-media descriptors и concrete transport providers.
//!
//! Crate владеет только typed identity/request/result, ephemeral secret scope,
//! redirect policy и process-local provider registry. Здесь намеренно нет HTTP
//! client/cache/prefetch implementation, yt-dlp DTO, demux, queue, player, UI
//! или decoder contracts.

#![forbid(unsafe_code)]

mod identity;
mod network;
mod provider;
mod registry;
mod request;
mod resource;
mod secret;

pub use identity::{
    MediaComponentIdentity, MediaComponentIdentityError, MediaComponentRole, SourceGeneration,
    SourceGenerationError, TransportProviderId, TransportProviderIdError,
};
pub use network::{
    RedirectAuthorization, RedirectHopCount, RedirectHopLimit, RedirectHopLimitError,
    RedirectOriginPolicy, RedirectPolicy, RedirectPolicyError, SecureRedirectPolicy,
};
pub use provider::{
    AuthenticationFailure, ProviderDescriptor, ProviderDescriptorError, ProviderOpenError,
    ProviderOpenOutput, ProviderRefreshError, RefreshFailure, RefreshSupport, TransportFailure,
    TransportProvider, UnsupportedTransportReason,
};
pub use registry::{
    ProviderContractViolation, TransportOpenError, TransportRefreshError, TransportRegistry,
    TransportRegistryError,
};
pub use request::{
    MediaPresentation, OpenedComponentIdentity, TransportOpenRequest, TransportOpenRequestError,
    TransportRefreshRequest, TransportRefreshRequestError,
};
pub use resource::{
    OpenedTransport, RefreshedTransport, TransportInput, TransportInputError, TransportSeekability,
};
pub use secret::{
    ScopedSecretRequestMaterial, SecretForwardingRequirement, SecretQueryOverride,
    SecretQueryOverrideError, SecretRequestContext, SecretRequestContextBuilder,
    SecretRequestPurpose, SecretRequestScope,
};

#[cfg(test)]
mod tests;
