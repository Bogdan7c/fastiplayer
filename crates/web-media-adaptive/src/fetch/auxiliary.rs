//! Вспомогательный (не видео) вид `AdaptiveHttpContext`: субтитры и подобные ресурсы.

use std::sync::{Arc, Mutex};

use super::AdaptiveHttpContext;
use crate::completed_resource_cache::CompletedResourceCache;

impl AdaptiveHttpContext {
    /// Представление контекста для вспомогательных ресурсов (субтитры и подобное).
    ///
    /// Делится всем, что безопасно делить: HTTP-сессия, cookies, scoped-секреты, политика
    /// редиректов/повторов, лимиты, токен отмены источника. НЕ делится тем, что принадлежит
    /// видео-пути: наблюдатель endpoint expiry отсутствует (401/403/404/410 вспомогательного
    /// ресурса не может взвести и заклеймить VOD endpoint recovery и не расходует его бюджет),
    /// а кэш завершённых VOD-ресурсов свежий (субтитры не вытесняют видео-байты и не читают
    /// чужие). Инвариант: сбой субтитров никогда не влияет на видео.
    #[must_use]
    pub fn for_auxiliary_resources(&self) -> Self {
        let mut auxiliary = self.clone();
        // Единственный путь публикации expiry идёт через этого наблюдателя.
        auxiliary.endpoint_expiry_observer = None;
        let cache_budget_bytes = self.lock_completed_resource_cache().budget_bytes();
        auxiliary.completed_resource_cache =
            Arc::new(Mutex::new(CompletedResourceCache::new(cache_budget_bytes)));
        auxiliary
    }
}
