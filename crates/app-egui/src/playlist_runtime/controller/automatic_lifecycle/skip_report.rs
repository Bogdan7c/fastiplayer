//! Сводка автоматических пропусков битых элементов очереди (UX edge cases, сессия 07).
//!
//! Цепочка пропусков растянута во времени: элемент B не открылся → controller планирует C →
//! C не открылся → … Сам алгоритм (какой элемент следующий, лимит шагов, без зацикливания)
//! принадлежит `playlist-core` и здесь не меняется. Этот модуль только **запоминает**, какие
//! элементы цепочка пропустила, и когда цепочка заканчивается, кладёт типизированный итог
//! [`AutomaticQueueNotice`] в «почтовый ящик». Приложение раз за кадр забирает итоги через
//! `PlaylistRuntime::drain_automatic_queue_notices` и само решает, как их показать.
//!
//! Цепочка начинается, когда текущий файл закончился или упал с ошибкой, либо когда после
//! рестарта не открылся восстановленный файл. Заканчивается она одним из трёх способов:
//! - успешно встал автоматически выбранный элемент → «пропущено N файлов»;
//! - остановка (политика `stop`, repeat-one, конец очереди) → итог по причине остановки;
//! - вмешательство пользователя (ручной Play/Next, открытие другого файла) → итог молча
//!   выбрасывается: пользователь сам сменил то, что играет.

use std::num::NonZeroUsize;

use playlist_core::{LocalLocator, PlaylistItemId, PlaylistLocator};

use super::AutomaticStopCause;
use crate::media_open::SafeMediaLabel;
use crate::playlist_runtime::controller::PlaylistController;
use crate::playlist_runtime::identity::PendingTargetOrigin;

/// Сколько имён пропущенных файлов показывать; остальные сворачиваются в «и ещё K».
///
/// Решение владельца (сессия 07): до трёх имён, чтобы плашка оставалась короткой.
pub(crate) const MAX_NAMED_SKIPPED_ITEMS: usize = 3;

/// Играло ли в очереди что-нибудь до начала цепочки.
///
/// Нужно, чтобы отличить «ни один файл очереди не открылся» (например, после рестарта все
/// файлы перемещены) от «файл доиграл, а дальше всё битое».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::playlist_runtime::controller) enum SkipChainStart {
    /// Цепочка началась после реально игравшего файла (конец или ошибка воспроизведения).
    AfterPlayback,
    /// Цепочка началась до первого успешного открытия (восстановление после рестарта).
    BeforeAnyPlayback,
}

/// Сколько элементов цепочка пропустила и первые из них по имени.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkippedItemsSummary {
    /// Всего пропущено элементов (хотя бы один: пустая сводка не создаётся).
    total: NonZeroUsize,
    /// Имена первых пропущенных элементов, не больше [`MAX_NAMED_SKIPPED_ITEMS`].
    ///
    /// Имён может быть меньше `total`: элемент мог исчезнуть из очереди до того, как
    /// controller узнал его имя, — такой элемент считается, но не называется.
    named_items: Vec<SafeMediaLabel>,
}

impl SkippedItemsSummary {
    /// Всего пропущено элементов.
    pub(crate) const fn total(&self) -> usize {
        self.total.get()
    }

    /// Имена первых пропущенных элементов в порядке пропуска.
    pub(crate) fn named_items(&self) -> &[SafeMediaLabel] {
        &self.named_items
    }

    /// Сколько пропущенных элементов не названо по имени («и ещё K»).
    pub(crate) fn unnamed_count(&self) -> usize {
        self.total().saturating_sub(self.named_items.len())
    }

    /// Сводка для тестов текста уведомлений вне controller-а.
    #[cfg(test)]
    pub(crate) fn from_test_parts(total: NonZeroUsize, named_items: &[&str]) -> Self {
        Self {
            total,
            named_items: named_items
                .iter()
                .map(|name| SafeMediaLabel::from_service_safe_label(name))
                .collect(),
        }
    }
}

/// Итог закончившейся цепочки, который стоит показать пользователю.
///
/// Тип не содержит готового текста: формулировки и способ показа выбирает приложение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutomaticQueueNotice {
    /// Битые элементы пропущены, следующий элемент успешно открылся.
    SkippedFailedItems { skipped: SkippedItemsSummary },
    /// Битые элементы пропущены, но дальше в очереди открыть нечего.
    SkippedToEndOfQueue { skipped: SkippedItemsSummary },
    /// До цепочки ничего не играло, и не открылся ни один элемент очереди.
    NothingInQueueOpened { skipped: SkippedItemsSummary },
    /// Очередь остановлена на битом элементе: так велит политика `stop` или repeat-one.
    StoppedOnFailedItem { failed_item: Option<SafeMediaLabel> },
}

/// Одна идущая цепочка пропусков.
#[derive(Debug)]
struct SkipChain {
    start: SkipChainStart,
    /// Сколько элементов цепочка уже пропустила.
    skipped_count: usize,
    /// Имена первых пропущенных элементов (не больше [`MAX_NAMED_SKIPPED_ITEMS`]).
    named_items: Vec<SafeMediaLabel>,
    /// Имя последнего неудачного элемента: на нём остановилась бы политика `stop`.
    last_failed_item: Option<SafeMediaLabel>,
}

impl SkipChain {
    const fn new(start: SkipChainStart) -> Self {
        Self {
            start,
            skipped_count: 0,
            named_items: Vec::new(),
            last_failed_item: None,
        }
    }

    fn record_failed_item(&mut self, label: Option<SafeMediaLabel>) {
        self.skipped_count = self.skipped_count.saturating_add(1);
        if let Some(label) = &label
            && self.named_items.len() < MAX_NAMED_SKIPPED_ITEMS
        {
            self.named_items.push(label.clone());
        }
        self.last_failed_item = label;
    }

    /// Пустая цепочка (ничего не пропущено) сводки не даёт.
    fn into_summary(self) -> Option<SkippedItemsSummary> {
        let total = NonZeroUsize::new(self.skipped_count)?;
        Some(SkippedItemsSummary {
            total,
            named_items: self.named_items,
        })
    }
}

/// Владелец цепочки пропусков и «почтового ящика» с итогами.
///
/// Хранится внутри `AutomaticLifecycle` контроллера; сам не читает очередь и не знает, как
/// итог будет показан. Все решения — чистые функции от фактов, которые передаёт controller.
#[derive(Debug, Default)]
pub(in crate::playlist_runtime::controller) struct AutomaticSkipReport {
    /// Идущая цепочка; `None` — сейчас ничего не пропускается.
    chain: Option<SkipChain>,
    /// Итоги закончившихся цепочек, ещё не забранные приложением.
    pending_notices: Vec<AutomaticQueueNotice>,
}

impl AutomaticSkipReport {
    /// Начинает новую цепочку; незаконченная прежняя молча выбрасывается.
    pub(super) fn begin_chain(&mut self, start: SkipChainStart) {
        self.chain = Some(SkipChain::new(start));
    }

    /// Запоминает элемент, который не открылся или упал во время воспроизведения.
    ///
    /// Если цепочки нет (её мог сбросить параллельный ручной intent, а план продолжения
    /// пережил это), начинается консервативная цепочка «после воспроизведения»: так не
    /// появится ложное «ни один файл очереди не открылся».
    pub(super) fn record_failed_item(&mut self, label: Option<SafeMediaLabel>) {
        self.chain
            .get_or_insert_with(|| SkipChain::new(SkipChainStart::AfterPlayback))
            .record_failed_item(label);
    }

    /// Автоматически выбранный элемент успешно открылся: цепочка закончилась удачей.
    pub(super) fn finish_with_installed_item(&mut self) {
        let Some(chain) = self.chain.take() else {
            return;
        };
        if let Some(skipped) = chain.into_summary() {
            self.pending_notices
                .push(AutomaticQueueNotice::SkippedFailedItems { skipped });
        }
    }

    /// Автоматическое воспроизведение остановилось; итог зависит от причины.
    ///
    /// `queue_item_count` — сколько элементов сейчас в очереди: «ни один не открылся»
    /// показывается, только если цепочка действительно перебрала их все.
    pub(super) fn finish_with_stop(&mut self, cause: AutomaticStopCause, queue_item_count: usize) {
        let Some(chain) = self.chain.take() else {
            return;
        };
        let notice = match cause {
            // Политика `stop` и repeat-one останавливаются на первом же битом элементе.
            AutomaticStopCause::ErrorPolicy | AutomaticStopCause::RepeatOneError => {
                (chain.skipped_count > 0).then(|| AutomaticQueueNotice::StoppedOnFailedItem {
                    failed_item: chain.last_failed_item.clone(),
                })
            }
            // Кандидаты закончились: либо всё битое, либо очередь кончилась после пропусков.
            AutomaticStopCause::AllCandidatesFailed { .. } | AutomaticStopCause::Domain(_) => {
                let covered_whole_queue = chain.skipped_count >= queue_item_count;
                let start = chain.start;
                chain.into_summary().map(|skipped| {
                    if start == SkipChainStart::BeforeAnyPlayback && covered_whole_queue {
                        AutomaticQueueNotice::NothingInQueueOpened { skipped }
                    } else {
                        AutomaticQueueNotice::SkippedToEndOfQueue { skipped }
                    }
                })
            }
            // Остановку вызвал пользователь или изменение очереди — сообщать нечего.
            AutomaticStopCause::ManualTraversalCancelled
            | AutomaticStopCause::StructuralInvalidation
            | AutomaticStopCause::DeferredCancelled => None,
            // Причину («Нет связи с сервером…») уже показывает ошибка player-а в центре;
            // элемент не битый, поэтому «пропущен»/«остановлено на файле» было бы ложью.
            AutomaticStopCause::NetworkLost => None,
        };
        if let Some(notice) = notice {
            self.pending_notices.push(notice);
        }
    }

    /// Пользователь вмешался (ручной Play/Next, открытие другого файла): итога не будет.
    pub(super) fn discard_chain(&mut self) {
        self.chain = None;
    }

    /// Отдаёт накопленные итоги ровно один раз.
    pub(super) fn drain_notices(&mut self) -> Vec<AutomaticQueueNotice> {
        std::mem::take(&mut self.pending_notices)
    }
}

impl PlaylistController {
    /// Начинает новую цепочку пропусков (см. [`SkipChainStart`]).
    pub(in crate::playlist_runtime::controller) fn begin_automatic_skip_chain(
        &mut self,
        start: SkipChainStart,
    ) {
        self.automatic_lifecycle.skip_report.begin_chain(start);
    }

    /// Запоминает неудачный элемент вместе с безопасным для показа именем.
    pub(in crate::playlist_runtime::controller) fn record_automatic_skip(
        &mut self,
        item_id: PlaylistItemId,
    ) {
        let label = self.skipped_item_label(item_id);
        self.automatic_lifecycle
            .skip_report
            .record_failed_item(label);
    }

    /// Закрывает цепочку остановкой с данной причиной.
    pub(in crate::playlist_runtime::controller) fn finish_automatic_skip_chain_with_stop(
        &mut self,
        cause: AutomaticStopCause,
    ) {
        let queue_item_count = self.queue.retained_item_count();
        self.automatic_lifecycle
            .skip_report
            .finish_with_stop(cause, queue_item_count);
    }

    /// Закрывает цепочку после успешной установки media.
    ///
    /// Удачей цепочки считается только автоматически выбранный элемент (продолжение очереди
    /// или восстановление после рестарта). Ручной выбор или явное открытие другого файла
    /// означают, что пользователь вмешался, и итог выбрасывается.
    pub(in crate::playlist_runtime::controller) fn settle_automatic_skip_chain_after_install(
        &mut self,
        committed_origin: Option<PendingTargetOrigin>,
    ) {
        match committed_origin {
            Some(PendingTargetOrigin::AutomaticAdvance | PendingTargetOrigin::RestoredCurrent) => {
                self.automatic_lifecycle
                    .skip_report
                    .finish_with_installed_item();
            }
            Some(
                PendingTargetOrigin::ExplicitRowPlay
                | PendingTargetOrigin::ManualNavigation { .. }
                | PendingTargetOrigin::ExplicitOpen,
            )
            | None => self.discard_automatic_skip_chain(),
        }
    }

    /// Выбрасывает идущую цепочку без итога.
    pub(in crate::playlist_runtime::controller) fn discard_automatic_skip_chain(&mut self) {
        self.automatic_lifecycle.skip_report.discard_chain();
    }

    /// Забирает итоги закончившихся цепочек ровно один раз.
    pub(crate) fn drain_automatic_queue_notices(&mut self) -> Vec<AutomaticQueueNotice> {
        self.automatic_lifecycle.skip_report.drain_notices()
    }

    /// Безопасное имя элемента для уведомления (решение владельца 1а: без пути к папке).
    ///
    /// Локальный файл — только имя файла. Ссылка или путь чужой платформы — название
    /// строки, как его показывает плейлист (title из metadata либо безопасный fallback).
    /// Элемент, которого уже нет в очереди, имени не получает.
    fn skipped_item_label(&self, item_id: PlaylistItemId) -> Option<SafeMediaLabel> {
        let item = self.queue.item(item_id)?;
        if let PlaylistLocator::Local(LocalLocator::Native(path)) = item.locator() {
            return Some(SafeMediaLabel::from_local_path(path));
        }
        let metadata = item.cached_metadata();
        // То же правило, что у строки плейлиста: пустой title не вытесняет fallback-имя.
        let row_title = metadata
            .title()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| metadata.fallback_display_name());
        Some(SafeMediaLabel::from_service_safe_label(row_title))
    }
}

#[cfg(test)]
mod tests;
