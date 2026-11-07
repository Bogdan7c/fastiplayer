//! Сборка `AdaptiveHttpContext` над выбранным транспортом.
//!
//! `new` (в родительском модуле) строит обычный HTTP-транспорт с cookie jar.
//! `with_transport` подключает внешний транспорт (мост в браузер) с теми же
//! проверками цели и секретов. Оба пути сходятся в `assemble`, поэтому слои
//! политики над транспортом у них одинаковые по построению.

use std::sync::{Arc, Mutex};

use source_core::{HttpRequestTargetError, SourceRuntimeConfig};
use web_media_transport_api::{SecretRequestPurpose, TransportOpenRequest};

use super::{AdaptiveHttpContext, AdaptiveTransportError};
use crate::completed_resource_cache::CompletedResourceCache;
use crate::transport::AdaptiveResourceTransport;
use crate::{AdaptiveRetryPolicy, AdaptiveTransportLimits};

impl AdaptiveHttpContext {
    /// Собирает context над внешним транспортом одного hop-а.
    ///
    /// Проверки те же, что у `new`: цель обязана быть HTTP(S), а scoped-секреты
    /// обязаны разрешать манифестный запрос к ней. Cookie jar и `HttpSourceSession`
    /// не создаются: у внешнего транспорта своё хранилище cookies (например,
    /// браузер). Поэтому секреты с cookies отклоняются как `SecretScopeRejected` —
    /// иначе они молча не дошли бы до сервера.
    ///
    /// Над транспортом остаются все слои: повторы, VOD-кэш, `RedirectPolicy`,
    /// заголовки/query секретов, наблюдатель истечения, `for_auxiliary_resources`.
    pub fn with_transport(
        request: TransportOpenRequest,
        transport: Arc<dyn AdaptiveResourceTransport>,
        source_config: &SourceRuntimeConfig,
        limits: AdaptiveTransportLimits,
        retry: AdaptiveRetryPolicy,
    ) -> Result<Self, AdaptiveTransportError> {
        let http_target = request
            .target()
            .as_http()
            .ok_or(AdaptiveTransportError::Target(
                HttpRequestTargetError::UnsupportedScheme,
            ))?;
        let initial_material = request
            .secrets()
            .material_for(http_target, SecretRequestPurpose::Manifest)
            .ok_or(AdaptiveTransportError::SecretScopeRejected)?;
        let carries_cookies = initial_material.cookies_for_request().is_some()
            || !initial_material.cookie_seeds_for_request().is_empty();
        if carries_cookies {
            return Err(AdaptiveTransportError::SecretScopeRejected);
        }
        Ok(Self::assemble(
            &request,
            transport,
            source_config,
            limits,
            retry,
        ))
    }

    /// Общая сборка неизменяемой политики context-а над уже выбранным транспортом.
    pub(super) fn assemble(
        request: &TransportOpenRequest,
        transport: Arc<dyn AdaptiveResourceTransport>,
        source_config: &SourceRuntimeConfig,
        limits: AdaptiveTransportLimits,
        retry: AdaptiveRetryPolicy,
    ) -> Self {
        // Бюджет VOD-кэша берётся из того же config-а, что и у обычного HTTP.
        let completed_cache_budget =
            usize::try_from(source_config.memory_cache_bytes()).unwrap_or(usize::MAX);
        Self {
            transport,
            secrets: request.secrets().clone(),
            redirects: request.redirects(),
            cancellation: request.cancellation().clone(),
            initial_generation: request.source_generation(),
            component: request.component().clone(),
            endpoint_expiry_observer: request.endpoint_expiry_observer().cloned(),
            expected_presentation: request.presentation(),
            limits,
            retry,
            completed_resource_cache: Arc::new(Mutex::new(CompletedResourceCache::new(
                completed_cache_budget,
            ))),
        }
    }
}
