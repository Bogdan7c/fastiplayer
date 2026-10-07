//! Ссылки, брошенные в окно (drag & drop): границы `PlaylistRuntime` для обоих мест броска.
//!
//! - Бросок на панель плейлиста идёт ТОЧНО тем же путём, что кнопка «Добавить URL»:
//!   [`PlaylistRuntime::append_playlist_url`] (классификация, topology-импорт со строкой
//!   прогресса, подтверждение сохранения «чувствительной» ссылки). Здесь только перевод
//!   отказа в безопасный текст для toast, потому что у броска нет поля ввода.
//! - Бросок на видео — «открыть как новую очередь»: прямая ссылка на media идёт тем же
//!   типизированным intent-ом замены очереди, что и остальные in-app открытия (с подтверждением,
//!   если очередь не пуста), а после допуска [`PlaylistRuntime::replace_queue_with_service_url`]
//!   коммитит очередь из одной ссылки. Ссылки с topology (yt-dlp) раскрывает
//!   `resolved_url_collection`; общий commit замены — `commit_dropped_replacement_drafts`.
//!
//! Исходная строка ссылки живёт только внутри типизированного locator-а и сервисов;
//! в тексты, `Debug` и логи этого модуля она не попадает.

use std::sync::Arc;

use playlist_core::{
    CachedPlaylistMetadata, PlaylistEntryDraft, PlaylistItemDraft, PlaylistItemId,
    PlaylistMediaKind,
};

use super::PlaylistRuntime;
use super::actions::UrlAppendActionOutcome;
use super::controller::{
    ControllerImportCommitError, ControllerImportCommitOutcome, ImportReplacementDisposition,
};
use super::url_draft_message::url_append_error_message;
use crate::url_service_adapter::StartupUrlLocator;

/// Текст, когда плейлист не принял ссылку из-за лимита числа строк.
const PLAYLIST_FULL_MESSAGE: &str = "Плейлист заполнен";

/// Итог добавления брошенной на панель ссылки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DroppedWebUrlAppendOutcome {
    /// Ссылка принята: добавлена, ждёт подтверждения или разбирается в фоне (строка прогресса).
    Accepted,
    /// Ссылка отклонена; текст безопасен (без исходной ссылки) и готов для показа.
    Refused { user_message: Arc<str> },
}

/// Итог замены очереди одной брошенной ссылкой.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ServiceUrlReplacementOutcome {
    /// Очередь заменена; `item` — единственная строка, которую надо играть.
    Replaced { item: PlaylistItemId },
    /// Состояние плейлиста ещё не загружено: очередь не тронута.
    LoadDecisionPending,
    /// Идёт установка media очередью: замена отложена, очередь не тронута.
    InstallInProgress,
    /// Runtime закрывается или owner отказал по инварианту: очередь не тронута.
    Rejected,
}

impl PlaylistRuntime {
    /// Бросок ссылки на панель плейлиста: тот же вход, что у кнопки «Добавить URL».
    pub(crate) fn append_dropped_web_url(
        &mut self,
        url_text: &str,
        yt_dlp_config: &fastiplayer_config::YtDlpConfig,
    ) -> DroppedWebUrlAppendOutcome {
        match self.append_playlist_url(url_text, yt_dlp_config) {
            Ok(UrlAppendActionOutcome::NoCapacity) => DroppedWebUrlAppendOutcome::Refused {
                user_message: PLAYLIST_FULL_MESSAGE.into(),
            },
            Ok(
                UrlAppendActionOutcome::Appended { .. }
                | UrlAppendActionOutcome::DeferredUntilStartupInstallResolution
                | UrlAppendActionOutcome::AwaitingSensitivePersistenceDecision
                | UrlAppendActionOutcome::ResolvingTopology,
            ) => DroppedWebUrlAppendOutcome::Accepted,
            Err(error) => DroppedWebUrlAppendOutcome::Refused {
                user_message: url_append_error_message(error),
            },
        }
    }

    /// Заменяет очередь одной ссылкой (после допуска замены), воспроизведение не запускает.
    ///
    /// Подтверждение замены непустой очереди уже получено выше по цепочке: сюда попадает
    /// только допущенный (`AdmittedUrlOpen`) locator. Строка получает «сырую» подпись
    /// сервиса (только домен), настоящие метаданные подтянет обычное обновление строк.
    pub(crate) fn replace_queue_with_service_url(
        &mut self,
        locator: &StartupUrlLocator,
    ) -> ServiceUrlReplacementOutcome {
        let Ok(playlist_locator) = locator.to_playlist_locator() else {
            tracing::warn!("Ссылка не превратилась в адрес строки очереди");
            return ServiceUrlReplacementOutcome::Rejected;
        };
        let draft = PlaylistEntryDraft::from(PlaylistItemDraft::url(
            playlist_locator,
            CachedPlaylistMetadata::new(locator.safe_label(), PlaylistMediaKind::Unknown),
        ));
        self.commit_dropped_replacement_drafts(vec![draft])
    }

    /// Общий commit замены очереди для брошенной ссылки (одна строка или целая коллекция).
    ///
    /// Владелец очереди и supersede-правила здесь одни: замена отменяет поздние результаты
    /// старых add/import/sibling-задач, затем `commit_import_replace` меняет очередь атомарно.
    /// `Replaced.item` — первая проигрываемая строка; запускает её вызывающий (Row Play).
    pub(super) fn commit_dropped_replacement_drafts(
        &mut self,
        drafts: Vec<PlaylistEntryDraft>,
    ) -> ServiceUrlReplacementOutcome {
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return ServiceUrlReplacementOutcome::Rejected;
        }
        // Замена очереди отменяет поздние результаты старых add/import/sibling-задач.
        self.supersede_manual_add_queue_generation();
        self.supersede_playlist_import_flow();
        let Some(controller) = self.controller.as_mut() else {
            return ServiceUrlReplacementOutcome::LoadDecisionPending;
        };
        let expected_revision = controller.view_snapshot().structural_revision();
        let dirty_before = controller.dirty_revision();
        let commit_result = controller.commit_import_replace(
            expected_revision,
            drafts,
            ImportReplacementDisposition::InteractiveDetached,
        );
        let outcome = match commit_result {
            Ok(ControllerImportCommitOutcome::Committed { allocated, .. }) => {
                match allocated.iter_playable_item_ids().next() {
                    Some(item) => ServiceUrlReplacementOutcome::Replaced { item },
                    None => ServiceUrlReplacementOutcome::Rejected,
                }
            }
            Ok(ControllerImportCommitOutcome::NoEntriesProvided) => {
                ServiceUrlReplacementOutcome::Rejected
            }
            Err(ControllerImportCommitError::InstallInProgress) => {
                ServiceUrlReplacementOutcome::InstallInProgress
            }
            Err(error) => {
                tracing::warn!(?error, "Замена очереди брошенной ссылкой отклонена");
                ServiceUrlReplacementOutcome::Rejected
            }
        };
        self.publish_controller_mutation_if_dirty(dirty_before);
        outcome
    }
}

#[cfg(test)]
mod tests;
