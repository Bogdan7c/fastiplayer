//! Operational open locator: «что реально открывать для этой строки очереди».
//!
//! Строка импортированной коллекции (например, плейлиста YouTube) хранит в
//! `item.locator()` ссылку на КОРЕНЬ коллекции (так устроена materialization S08),
//! а собственную identity ролика — в durable payload (`ServicePayload`).
//! Любой open/probe по сети обязан идти через [`operational_open_locator`],
//! иначе yt-dlp получит ссылку на весь плейлист и будет резолвить все его
//! элементы вместо одного ролика.
//!
//! Модуль владеет только выбором locator-а. Разбор bytes payload-а принадлежит
//! `service-ytdlp`, app их не парсит. `item.locator()` остаётся identity строки
//! (stale-guard метаданных, ключ resume-checkpoint) и здесь не меняется.

use std::sync::Arc;

use playlist_core::{
    DurableReopenLocator, PlaylistItem, PlaylistLocator, SecretUrlLocator,
    ServiceReopenMaterialKind,
};
use service_ytdlp::{
    YtDlpDurableReopenDecodeError, YtDlpDurableReopenMaterialKind, YtDlpDurableReopenPayloadInput,
    decode_yt_dlp_durable_reopen_payload,
};
use thiserror::Error;

use super::controller::PlannedPlaylistInstall;
use super::{PlaylistMediaOpenGateError, PlaylistRuntime};

/// Почему у строки нет locator-а, пригодного для открытия отдельного ролика.
///
/// `Copy`, потому что вкладывается в `Copy`-ошибки media-open gate. Тексты
/// безопасны для логов и UI: ссылки и идентификаторы в них не попадают.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum OperationalOpenLocatorError {
    /// Service-owned payload отказался превращаться в ссылку на ролик.
    #[error("{0}")]
    Service(#[from] YtDlpDurableReopenDecodeError),
    /// Категория payload-а не является stable identity (конструктор payload-а такое не создаёт).
    #[error("Сохранённые данные ролика не годятся для повторного открытия")]
    NotStableIdentity,
}

/// Выбирает locator, который нужно передавать сетевому open/probe для строки очереди.
///
/// - service-owned payload ролика коллекции -> собственная identity ролика;
/// - всё остальное (одиночная ссылка, локальный файл, импорт без payload-а) ->
///   прежний `item.locator()`.
///
/// Extractor-only identity даёт типизированный отказ, а НЕ откат на ссылку плейлиста:
/// откат вернул бы исходный баг (минуты резолва всей коллекции).
pub(crate) fn operational_open_locator(
    item: &PlaylistItem,
) -> Result<PlaylistLocator, OperationalOpenLocatorError> {
    let Some(durable_payload) = item.durable_payload() else {
        return Ok(item.locator().clone());
    };
    let DurableReopenLocator::ServicePayload(service_payload) = durable_payload.reopen_locator()
    else {
        return Ok(item.locator().clone());
    };

    let material_kind = match service_payload.material_kind() {
        ServiceReopenMaterialKind::StableWebpageIdentity => {
            YtDlpDurableReopenMaterialKind::StableWebpageIdentity
        }
        ServiceReopenMaterialKind::StableOriginalIdentity => {
            YtDlpDurableReopenMaterialKind::StableOriginalIdentity
        }
        ServiceReopenMaterialKind::StableExtractorIdentity => {
            YtDlpDurableReopenMaterialKind::StableExtractorIdentity
        }
        // Ephemeral-категории отвергаются на этапе создания payload-а; здесь fail-closed.
        _ => return Err(OperationalOpenLocatorError::NotStableIdentity),
    };
    let entry_locator = decode_yt_dlp_durable_reopen_payload(YtDlpDurableReopenPayloadInput {
        service_owner: service_payload.service_owner(),
        payload_version: service_payload
            .payload_version()
            .expose_value_for_persistence(),
        material_kind,
        payload_bytes: service_payload.expose_payload_for_reopen(),
    })?;
    let entry_url =
        SecretUrlLocator::from_reopenable_url(entry_locator.expose_secret_for_persistence())
            .map_err(|_| YtDlpDurableReopenDecodeError::InvalidStoredLocator)?;
    Ok(PlaylistLocator::Url(entry_url))
}

impl PlaylistRuntime {
    /// Человеческая причина, если у планируемой строки нет locator-а для открытия.
    ///
    /// `None` — строка открываема (или план устарел): бейдж остаётся прежним общим.
    pub(crate) fn operational_open_refusal_summary(
        &self,
        install: &PlannedPlaylistInstall,
    ) -> Option<Arc<str>> {
        match self.media_open_intent_for_planned_install(install) {
            Err(PlaylistMediaOpenGateError::OperationalLocatorRefused(refusal)) => {
                Some(Arc::from(refusal.to_string()))
            }
            Ok(_) | Err(_) => None,
        }
    }
}

#[cfg(test)]
pub(crate) mod entry_fixtures_tests;
#[cfg(test)]
mod tests;
