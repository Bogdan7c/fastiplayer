//! Ссылка, брошенная на видео, после получения её структуры (решение владельца 9, сессия 12).
//!
//! Тот же job, что у «Добавить URL» (`url_import`: классификация, строка прогресса, отмена,
//! типизированные причины отказа), но с назначением
//! [`PlaylistUrlImportDestination::ReplaceAfterConfirmation`]: успешный результат НЕ
//! дописывается в очередь, а превращается в [`ResolvedUrlCollection`] и ждёт замены очереди.
//!
//! # Поток
//! 1. [`PlaylistRuntime::start_dropped_web_url_replacement`] классифицирует ссылку и запускает
//!    job (yt-dlp) либо сообщает, что topology не нужна (прямая ссылка на media).
//! 2. Worker разбирает структуру; drain владельца кладёт коллекцию в его слот.
//! 3. Оболочка забирает её через [`PlaylistRuntime::take_resolved_url_collection`] и ведёт
//!    через общий admission замены очереди: пустая очередь — замена сразу, иначе подтверждение
//!    с подписью «N роликов с домена».
//! 4. После допуска [`PlaylistRuntime::replace_queue_with_resolved_url_collection`]
//!    коммитит ровно эти записи в исходном порядке; первую играет оболочка.
//!
//! # Устаревший результат
//! Слот коллекции живёт внутри владельца URL-импорта и очищается тем же supersede, что и сам
//! job: новый Add URL, любое другое открытие/замена очереди, отмена кнопкой «Отменить»,
//! shutdown. Ответ на уже показанное подтверждение защищён общим slot-ом подтверждений
//! (новый intent его заменяет). Дополнительно `AppState` перед применением отбрасывает
//! результат, если уже идёт открытие файла или импорт плейлиста.
//!
//! Исходные ссылки записей живут только внутри типизированных draft-ов; в `Debug`, тексты
//! и логи этого модуля они не попадают.

use std::fmt;
use std::sync::Arc;

use playlist_core::{PlaylistEntryDraft, PlaylistImportEntryDraft};

use super::actions::{NotUrlInputHint, UrlAppendValidationError};
use super::dropped_web_url::ServiceUrlReplacementOutcome;
use super::import_transaction::{PlaylistImportCapacityTruncation, PlaylistImportDraft};
use super::replacement_confirmation::AdmittedResolvedUrlCollection;
use super::url_draft_message::url_append_error_message;
use super::url_import::{PlaylistUrlImportDestination, PlaylistUrlImportStartError};
use super::{InAppQueueReplacementIntent, PlaylistRuntime};
use crate::url_service_adapter::{StartupUrlClassification, classify_startup_url};

/// Текст, когда коллекция не поместилась в лимит очереди и часть записей отброшена.
const COLLECTION_TRUNCATED_MESSAGE: &str =
    "Все ролики по ссылке не поместились в плейлист: добавлены первые";

/// Разобранная структура ссылки, ожидающая замены очереди.
///
/// Владеет ID-less записями (уже усечёнными под лимит очереди целыми записями) и безопасными
/// фактами для подтверждения: число роликов и домен.
pub(crate) struct ResolvedUrlCollection {
    /// Принятые целые записи в порядке источника.
    entries: Vec<PlaylistImportEntryDraft>,
    /// Не поместившийся хвост; `None` — поместилось всё.
    capacity_truncation: Option<PlaylistImportCapacityTruncation>,
    /// Сколько ссылок требует подтверждения сохранения «чувствительного» адреса.
    sensitive_durable_locator_count: usize,
    /// Безопасный домен («youtube.com»); `None` — у ссылки нет домена.
    display_host: Option<Arc<str>>,
}

impl ResolvedUrlCollection {
    /// Строит коллекцию из draft-а topology job-а; capacity-политика та же, что у S08.
    pub(super) fn from_resolved_draft(
        draft: PlaylistImportDraft,
        display_host: Option<Arc<str>>,
    ) -> Self {
        let prefix = draft.into_replacement_prefix();
        Self {
            entries: prefix.entries,
            capacity_truncation: prefix.capacity_truncation,
            sensitive_durable_locator_count: prefix.sensitive_durable_locator_count,
            display_host,
        }
    }

    /// Сколько роликов (строк очереди) будет создано; для подписи подтверждения.
    pub(crate) fn item_count(&self) -> usize {
        self.entries
            .iter()
            .map(PlaylistImportEntryDraft::retained_item_count)
            .sum()
    }

    /// Безопасный домен ссылки для подписи подтверждения.
    pub(crate) fn display_host(&self) -> Option<&str> {
        self.display_host.as_deref()
    }

    /// Нужно ли подтверждение сохранения адреса с секретами (тот же D15-признак, что у Add URL).
    pub(crate) const fn requires_sensitive_persistence_acknowledgement(&self) -> bool {
        self.sensitive_durable_locator_count > 0
    }

    /// Пустая ли коллекция (структура разобрана, но роликов нет).
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Превращает записи в draft-ы очереди; ошибка — недоказуемый locator, очередь не трогается.
    fn into_queue_drafts(
        self,
    ) -> Result<
        (
            Vec<PlaylistEntryDraft>,
            Option<PlaylistImportCapacityTruncation>,
        ),
        playlist_core::PlaylistImportMaterializationError,
    > {
        let drafts = self
            .entries
            .into_iter()
            .map(PlaylistImportEntryDraft::into_queue_draft)
            .collect::<Result<Vec<_>, _>>()?;
        Ok((drafts, self.capacity_truncation))
    }
}

impl fmt::Debug for ResolvedUrlCollection {
    /// Только счётчик: записи содержат исходные ссылки.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedUrlCollection")
            .field("item_count", &self.item_count())
            .finish()
    }
}

/// Итог запуска «ссылка на видео».
#[derive(Debug)]
pub(crate) enum DroppedWebUrlReplacementStart {
    /// Структуру получает фоновый job (строка прогресса уже видна); результат придёт через
    /// [`PlaylistRuntime::take_resolved_url_collection`].
    ResolvingTopology,
    /// Topology не нужна (прямая ссылка на media): оболочка заменяет очередь одной ссылкой
    /// прежним путём через этот intent.
    SingleLink(InAppQueueReplacementIntent),
    /// Ссылка отклонена; текст безопасен (без исходной ссылки) и готов для показа.
    Refused { user_message: Arc<str> },
}

impl PlaylistRuntime {
    /// Запускает «ссылка на видео»: та же классификация и тот же job, что у «Добавить URL».
    ///
    /// Очередь и плеер не меняются. Прежний URL-job/preview/подтверждение заменяются новым.
    pub(crate) fn start_dropped_web_url_replacement(
        &mut self,
        url_text: &str,
        yt_dlp_config: &fastiplayer_config::YtDlpConfig,
    ) -> DroppedWebUrlReplacementStart {
        let refused = |error| DroppedWebUrlReplacementStart::Refused {
            user_message: url_append_error_message(error),
        };
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return refused(UrlAppendValidationError::RuntimeShuttingDown);
        }
        let locator = match classify_startup_url(url_text.trim()) {
            StartupUrlClassification::Supported(locator) => locator,
            StartupUrlClassification::Unsupported { reason } => {
                return refused(UrlAppendValidationError::Unsupported {
                    safe_error: reason.safe_error(),
                });
            }
            StartupUrlClassification::NotUrl => {
                return refused(UrlAppendValidationError::NotUrl(
                    NotUrlInputHint::Unrecognized,
                ));
            }
        };
        // Как у Add URL: только yt-dlp locator имеет topology; direct media идёт без неё.
        let Some(yt_dlp_locator) = locator.yt_dlp_topology_locator().cloned() else {
            return DroppedWebUrlReplacementStart::SingleLink(
                InAppQueueReplacementIntent::service_url(locator),
            );
        };
        // Новый intent supersede-ит старый URL-job/preview; старое подтверждение к нему не относится.
        self.supersede_playlist_import_flow();
        self.replacement_confirmation.cancel();
        let started = self.start_playlist_url_import(
            yt_dlp_locator,
            yt_dlp_config.clone(),
            usize::from(locator.requires_sensitive_persistence_acknowledgement()),
            locator.display_host(),
            PlaylistUrlImportDestination::ReplaceAfterConfirmation,
        );
        match started {
            Ok(()) => DroppedWebUrlReplacementStart::ResolvingTopology,
            Err(PlaylistUrlImportStartError::GenerationExhausted) => {
                refused(UrlAppendValidationError::TopologyGenerationExhausted)
            }
            Err(PlaylistUrlImportStartError::WorkerUnavailable) => {
                refused(UrlAppendValidationError::TopologyWorkerUnavailable)
            }
        }
    }

    /// Забирает разобранную коллекцию ровно один раз (`None` — нечего применять или она устарела).
    pub(crate) fn take_resolved_url_collection(&mut self) -> Option<ResolvedUrlCollection> {
        self.url_import.take_resolved_replacement()
    }

    /// Замена очереди допущенной коллекцией; воспроизведение запускает вызывающий.
    ///
    /// Ownership: подтверждение уже получено выше по цепочке. Здесь только commit записей в
    /// исходном порядке; `Replaced.item` — первая проигрываемая строка. Усечение по лимиту
    /// очереди не молчит: пользователь видит безопасное сообщение в области проблем.
    pub(crate) fn replace_queue_with_resolved_url_collection(
        &mut self,
        admitted: AdmittedResolvedUrlCollection,
    ) -> ServiceUrlReplacementOutcome {
        let (drafts, capacity_truncation) = match admitted.into_collection().into_queue_drafts() {
            Ok(materialized) => materialized,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "Коллекция по ссылке не превратилась в строки очереди"
                );
                return ServiceUrlReplacementOutcome::Rejected;
            }
        };
        let outcome = self.commit_dropped_replacement_drafts(drafts);
        if matches!(outcome, ServiceUrlReplacementOutcome::Replaced { .. })
            && capacity_truncation.is_some()
        {
            self.set_playlist_safe_feedback(COLLECTION_TRUNCATED_MESSAGE);
        }
        outcome
    }
}
