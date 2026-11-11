//! Открытие поколения: опрос ждущего поколения, загрузка готового медиа в плеер и выдача
//! запросов байтов файла странице (`fetch_range`).
//!
//! Вынесено из `session_media.rs` (предел 800 строк) без изменения поведения. Состояние
//! (`pending`, `loaded`, `outbound`) по-прежнему принадлежит [`SessionMedia`].

use std::time::Instant;

use browser_bridge_protocol::SessionCloseReason;
use browser_bridge_protocol::session::player::StreamRejection;
use player_core::{PreparedInitialPosition, PreparedMedia};
use pushed_media_source::RangeRequest;
use tracing::{error, info, warn};

use super::generation_feed::{LoadedFeed, OpenStep, in_issue_order};
use super::generation_placement::PageAvailability;
use super::generation_rejection::ViewerAwareness;
use super::generation_start::AfterLoad;
use super::{LoadedGeneration, PendingGeneration, SessionMedia};
use crate::browser_session::outbound::RangeQueueOutcome;
use crate::browser_session::playback::LoadStart;
use crate::browser_session::rejection::FailureResponse;

impl SessionMedia {
    /// Спрашивает ждущее поколение, если пора (срок подсказки или пришли байты).
    pub(super) fn poll_pending(&mut self, now: Instant) {
        let bytes_arrived = std::mem::take(&mut self.bytes_arrived);
        // Страница ушла (доигрываем) — байтов у цели больше не будет (ворота 04d).
        let page = if self.drain.is_some() {
            PageAvailability::Gone
        } else {
            PageAvailability::Listening
        };
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        pending.placement.arm(now);
        let due = bytes_arrived || pending.retry_at.is_none_or(|retry_at| retry_at <= now);
        if !due {
            return;
        }
        match pending
            .feed
            .poll_open(&mut pending.placement, now, page, self.playback.as_ref())
        {
            OpenStep::Waiting(retry_after) => {
                pending.retry_at = Some(now + retry_after);
            }
            OpenStep::Ready {
                prepared,
                feed,
                after_load,
            } => {
                if let Some(pending) = self.pending.take() {
                    self.load(&pending, *prepared, feed, after_load);
                }
            }
            OpenStep::Failed { class, response } => {
                if let Some(pending) = self.pending.take() {
                    // Только класс ошибки: текст несёт подробности байтов страницы.
                    warn!(
                        failure_class = class,
                        generation = pending.generation.get(),
                        "Stream generation did not open"
                    );
                    match response {
                        FailureResponse::RejectGeneration(reason) => {
                            self.reject(pending.generation, reason, ViewerAwareness::Unaware);
                        }
                        FailureResponse::CloseSession(reason) => {
                            self.close(&pending.session, reason);
                        }
                    }
                }
            }
            OpenStep::Unplayable => {
                // Дефект сборки, а не данных страницы; видео — обратно странице.
                if let Some(pending) = self.pending.take() {
                    self.reject(
                        pending.generation,
                        StreamRejection::UnsupportedCodec,
                        ViewerAwareness::Unaware,
                    );
                }
            }
        }
    }

    /// Готовое медиа → плеер; затем то, что нужно для старта с нужного места.
    fn load(
        &mut self,
        pending: &PendingGeneration,
        prepared: PreparedMedia,
        feed: LoadedFeed,
        after_load: AfterLoad,
    ) {
        // Страница на паузе — загрузка сразу на паузе: «играть, потом пауза» дало бы
        // мелькнуть звуку. Страница играет — старт сразу (решение 99).
        let start = if self.page_wants_play {
            LoadStart::Playing
        } else {
            LoadStart::Paused
        };
        // Загрузка забирает `prepared`: где оно стоит, запоминаем до неё.
        let positioned_at = match prepared.prepared_initial_position() {
            PreparedInitialPosition::PositionedAt {
                target_position, ..
            } => Some(target_position.as_duration()),
            PreparedInitialPosition::Beginning => None,
        };
        let load_ticket = match self.playback.load(prepared, start) {
            Ok(load_ticket) => load_ticket,
            Err(load_error) => {
                // Плеер не взял команду (поток плеера встал): честнее вернуть видео
                // странице, чем показывать ей вечное ожидание.
                error!(%load_error, "The player did not accept the browser stream");
                self.reject(
                    pending.generation,
                    StreamRejection::UnsupportedCodec,
                    ViewerAwareness::Unaware,
                );
                return;
            }
        };
        info!(
            generation = pending.generation.get(),
            "Browser stream generation loaded"
        );
        self.loaded = Some(LoadedGeneration {
            generation: pending.generation,
            load_ticket,
            feed,
            fault_reported: false,
            page_state_reconciled: false,
            // Файл перематывается, когда плеер его покажет: до установки команда
            // досталась бы прежнему медиа.
            seek_when_shown: match after_load {
                AfterLoad::Nothing => None,
                AfterLoad::SeekTo(target) => Some(target),
            },
            start: pending.placement.named_start(),
            positioned_at,
            unanswered_seek: None,
        });
    }

    /// Запросы байтов файла → странице (`fetch_range`), сразу по пробуждению.
    ///
    /// Забираются у ждущего файла (первый запрос открытия выдаёт первый `poll_open`) и
    /// у загруженного (чтение, перемотка, повтор после сбоя сети).
    pub(super) fn send_range_requests(&mut self) {
        let pending_requests = self
            .pending
            .as_ref()
            .map(|pending| pending.feed.take_range_requests())
            .unwrap_or_default();
        let loaded_requests = self
            .loaded
            .as_ref()
            .map(|loaded| loaded.feed.take_range_requests())
            .unwrap_or_default();
        // Странице запросы уходят в порядке выдачи: номера сквозные на сеанс, а ждущий
        // файл мог выдать запрос раньше, чем загруженный — свой последний.
        let all_requests = in_issue_order(pending_requests.into_iter().chain(loaded_requests));
        for request in all_requests {
            self.queue_range_request(request);
        }
    }

    /// Один запрос байтов — в очередь к странице. Отказ очереди (недостижимый при честной
    /// работе, см. `MAX_PENDING_RANGE_REQUESTS`) — внутренняя ошибка: файл ждал бы байтов,
    /// о которых страница не знает, поэтому сеанс закрывается как нарушение протокола, а
    /// в журнал идёт причина.
    fn queue_range_request(&mut self, request: RangeRequest) {
        let outcome = self.outbound.request_range(request);
        if outcome == RangeQueueOutcome::Queued {
            return;
        }
        error!(
            ?outcome,
            "A range request could not be queued for the page; closing the session"
        );
        if let Some(session) = self.current_session.clone() {
            self.close(&session, SessionCloseReason::ProtocolViolation);
        }
    }
}
