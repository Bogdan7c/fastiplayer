//! Переподключение HTTP Range чтения после обрыва связи (сессия 16).
//!
//! Дочерний модуль `http`: владеет циклом повторов одного логического чтения и
//! проверкой «тот же ли файл» у каждого `206`. Политика пауз и бюджета живёт в
//! [`crate::http_reconnect`], физический запрос — в родительском
//! [`HttpRangeSource::read_range_once`].

use std::io::Read;
use std::time::Instant;

use reqwest::header::HeaderMap;

use super::{HttpRangeSource, validators_from_headers};
use crate::http_reconnect::{HttpReconnectDecision, HttpReconnectSchedule, wait_before_reconnect};
use crate::{
    CancellationToken, HttpRepresentationChange, SecretHttpUrl, SourceError, SourceResult,
};

impl HttpRangeSource {
    /// Читает один bounded range и переживает временный обрыв связи.
    ///
    /// При временном сбое (см. [`SourceError::is_transient_network_failure`]) запрос
    /// повторяется по [`HttpReconnectSchedule`]: уже полученные байты сохраняются, и
    /// следующий запрос продолжает ровно с первого недостающего байта. Ошибки, которые
    /// повтором не лечатся (404, 403, «файл изменился», отмена), возвращаются сразу.
    /// После исчерпания бюджета возвращается последняя ошибка.
    pub(super) fn read_range_with_reconnect(
        &mut self,
        offset: u64,
        output: &mut [u8],
        cancellation: &CancellationToken,
    ) -> SourceResult<usize> {
        let mut schedule = HttpReconnectSchedule::new(self.reconnect_policy);
        let mut filled_bytes = 0_usize;

        loop {
            let resume_offset = offset.saturating_add(filled_bytes as u64);
            let failure = match self.read_range_once(
                resume_offset,
                &mut output[filled_bytes..],
                cancellation,
            ) {
                Ok(bytes_read) => {
                    if schedule.failed_attempts() > 0 {
                        tracing::info!(
                            source = %self.url,
                            offset,
                            reconnect_attempts = schedule.failed_attempts(),
                            "HTTP Range чтение восстановилось после обрыва связи"
                        );
                    }
                    return Ok(filled_bytes.saturating_add(bytes_read));
                }
                Err(failure) => failure,
            };
            filled_bytes = filled_bytes.saturating_add(failure.received_bytes);
            let error = failure.error;
            self.record_range_error(&error);
            if !error.is_transient_network_failure() {
                return Err(error);
            }

            match schedule.record_failure(Instant::now(), error.http_retry_after()) {
                HttpReconnectDecision::GiveUp => {
                    tracing::warn!(
                        source = %self.url,
                        offset,
                        reconnect_attempts = schedule.failed_attempts(),
                        reconnect_budget_ms = self.reconnect_policy.budget().as_millis(),
                        error = %error,
                        "HTTP Range чтение: связь не восстановилась за бюджет ожидания"
                    );
                    return Err(error);
                }
                HttpReconnectDecision::RetryAfter(delay) => {
                    self.diagnostics.reconnects = self.diagnostics.reconnects.saturating_add(1);
                    tracing::warn!(
                        source = %self.url,
                        offset,
                        resume_offset = offset.saturating_add(filled_bytes as u64),
                        reconnect_attempt = schedule.failed_attempts(),
                        delay_ms = delay.as_millis(),
                        error = %error,
                        "HTTP Range чтение оборвалось; повтор с того же байта после паузы"
                    );
                    wait_before_reconnect(delay, cancellation)?;
                }
            }
        }
    }

    /// Проверяет, что очередной `206` относится к тому же файлу, что и первый.
    ///
    /// Без этой проверки повтор после обрыва мог бы склеить начало старого файла с
    /// продолжением нового, если файл на сервере успели заменить. Сравниваются только
    /// поля, которые есть у обеих сторон: CDN, который просто не прислал validator в
    /// одном из ответов, не доказывает изменение файла и не должен ронять просмотр.
    pub(super) fn ensure_same_representation(
        &self,
        response_total_length: Option<u64>,
        response_headers: &HeaderMap,
    ) -> SourceResult<()> {
        if let (Some(expected), Some(observed)) = (self.content_length, response_total_length)
            && expected != observed
        {
            return Err(SourceError::HttpRepresentationChanged {
                reason: HttpRepresentationChange::TotalLength,
            });
        }

        let observed = validators_from_headers(response_headers);
        let validator_changed = |expected: &Option<String>, observed: &Option<String>| matches!((expected, observed), (Some(expected), Some(observed)) if expected != observed);
        if validator_changed(&self.validators.etag, &observed.etag)
            || validator_changed(&self.validators.last_modified, &observed.last_modified)
        {
            return Err(SourceError::HttpRepresentationChanged {
                reason: HttpRepresentationChange::Validators,
            });
        }
        Ok(())
    }
}

/// Неудачная попытка Range чтения вместе с уже полученными байтами.
#[derive(Debug)]
pub(super) struct RangeReadFailure {
    /// Сколько байт с начала запрошенного range записано в buffer до сбоя.
    pub(super) received_bytes: usize,
    /// Причина сбоя.
    pub(super) error: SourceError,
}

impl RangeReadFailure {
    /// Сбой после частично полученного body.
    pub(super) const fn after_bytes(received_bytes: usize, error: SourceError) -> Self {
        Self {
            received_bytes,
            error,
        }
    }
}

impl From<SourceError> for RangeReadFailure {
    /// Сбой до первого байта body (подключение, статус, заголовки).
    fn from(error: SourceError) -> Self {
        Self::after_bytes(0, error)
    }
}

/// Читает ровно ожидаемое количество bytes из response body в caller buffer.
///
/// При обрыве возвращает, сколько байт уже записано: они валидны, и повтор
/// продолжит с первого недостающего байта, а не скачает chunk заново.
pub(super) fn read_response_body_into(
    url: &SecretHttpUrl,
    mut response: reqwest::blocking::Response,
    offset: u64,
    output: &mut [u8],
    cancellation: &CancellationToken,
) -> Result<usize, RangeReadFailure> {
    let expected_length = output.len();
    let mut total_read = 0_usize;

    while total_read < expected_length {
        if cancellation.is_cancelled() {
            return Err(RangeReadFailure::after_bytes(
                total_read,
                SourceError::Cancelled,
            ));
        }

        match response.read(&mut output[total_read..]) {
            Ok(0) => break,
            Ok(bytes_read) => {
                total_read = total_read.saturating_add(bytes_read);
            }
            Err(source) if source.kind() == std::io::ErrorKind::TimedOut => {
                return Err(RangeReadFailure::after_bytes(
                    total_read,
                    SourceError::HttpTimeout {
                        operation: "range-read",
                        url: url.clone(),
                    },
                ));
            }
            Err(source) => {
                return Err(RangeReadFailure::after_bytes(
                    total_read,
                    SourceError::HttpBodyRead {
                        operation: "range-read",
                        url: url.clone(),
                        source,
                    },
                ));
            }
        }
    }

    if total_read != expected_length {
        return Err(RangeReadFailure::after_bytes(
            total_read,
            SourceError::UnexpectedEof {
                offset,
                expected_bytes: expected_length,
                actual_bytes: total_read,
            },
        ));
    }

    Ok(total_read)
}
