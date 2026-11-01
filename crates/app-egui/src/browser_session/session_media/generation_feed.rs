//! Откуда медиа-часть берёт содержимое поколения: «сегменты» или «файл» (03c).
//!
//! Поколение страницы бывает двух видов, и главному циклу медиа они нужны по-разному:
//!
//! | | сегменты MSE | файл `<video src>` |
//! |---|---|---|
//! | открытие | `poll_open` потока | `poll_open` файла (первый вызов ставит запрос) |
//! | `PreparedMedia` | демуксер потока (+ живая шкала) | открытый блокирующий демуксер → фоновый поток с перемоткой |
//! | пока играет | монитор: запас, «полон», просьбы перемотки страницы | запросы байтов `fetch_range` странице |
//! | `buffer_state` странице | да | нет (страница ничего не качает сама) |
//!
//! Различие выражено типами [`PendingFeed`] (ещё открывается) и [`LoadedFeed`] (в
//! плеере), а не флагами: `SessionMedia` спрашивает у них «открылось?», «какие запросы
//! байтов?», «что сказать монитору?» и не знает, что внутри.

use std::time::{Duration, Instant};

use media_prefetch::PrefetchConfig;
use media_source_open::prepared_web_media::{
    PreparedWebMediaAttachments, PreparedWebMediaSeekAttachment, compose_prepared_web_media,
};
use media_source_open::progressive_player_demux::into_player_demuxer;
use player_core::{PreparedInitialPositionError, PreparedMedia, PreparedMediaTimelineModeError};
use pushed_media_source::{
    BufferStatus, FileOpenProgress, FileSeekability, OpenProgress, OpenedPushedFile,
    OpenedPushedStream, PageSeekRequest, PushedFileOpener, PushedFileRequests, PushedStreamMonitor,
    PushedStreamOpener, RangeRequest,
};
use tracing::error;
use web_media_transport_api::TransportSeekability;

use super::BROWSER_STREAM_LABEL;
use super::generation_placement::{
    GenerationPlacement, PageAvailability, PendingOpenState, PlacementVerdict,
};
use super::generation_start::{
    AfterLoad, GenerationStart, is_positionable, position_segments_demuxer,
};
use super::scrub_range::{SegmentsTimeline, segments_downloaded_range};
use crate::browser_session::intake_notice::GenerationSource;
use crate::browser_session::rejection::{
    FailureResponse, response_to_file_failure, response_to_stream_failure,
};
use crate::downloaded_range::DownloadedRange;

/// Как часто спрашивать открывающийся файл. Открытие идёт в своём потоке источника и
/// главный цикл о его конце не уведомляет (уведомляет только о новых запросах), а
/// байты страницы будят цикл подсказкой «легли байты». 25 мс — запас от пропущенного
/// пробуждения: открытие заметно быстрее, чем человек нажал «играть», а холостых
/// проходов за секунду не больше сорока и только пока файл открывается.
const FILE_OPEN_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Ждущее поколение: сторона открытия и то, что понадобится после открытия.
pub(super) enum PendingFeed {
    /// Сегменты MSE.
    Segments {
        /// Сторона открытия потока.
        opener: PushedStreamOpener,
        /// Монитор буфера (переходит к загруженному поколению).
        monitor: PushedStreamMonitor,
        /// Поток уже открыт, но ворота поколения его ещё держат: у цели нет данных
        /// (04d, путь C — [`super::generation_placement`]). Открыть поток второй раз
        /// нельзя, поэтому открытый ждёт здесь.
        opened: Option<OpenedPushedStream>,
    },
    /// Файл `<video src>`.
    File {
        /// Сторона открытия файла.
        opener: PushedFileOpener,
        /// Очередь запросов байтов (переходит к загруженному поколению).
        requests: PushedFileRequests,
        /// Окно предзагрузки для фонового демуксера файла.
        prefetch: PrefetchConfig,
    },
}

/// Содержимое поколения, загруженного в плеер.
pub(super) enum LoadedFeed {
    /// Сегменты MSE: монитор буфера.
    Segments {
        /// Монитор буфера потока.
        monitor: PushedStreamMonitor,
        /// Что скачано (вопрос к полке; у эфира — неизвестно), для живого скраба окна.
        downloaded_range: DownloadedRange,
    },
    /// Файл: запросы байтов, которые источник продолжает выдавать (чтение, перемотка).
    File {
        /// Очередь запросов байтов.
        requests: PushedFileRequests,
    },
}

/// Что показал очередной опрос ждущего поколения.
pub(super) enum OpenStep {
    /// Ещё открывается: спросить снова через этот срок (или раньше, по байтам).
    Waiting(Duration),
    /// Открылось: медиа для плеера и содержимое загруженного поколения.
    Ready {
        /// Что отдать плееру (у сегментов — уже на стартовой позиции поколения).
        prepared: Box<PreparedMedia>,
        /// Что медиа-часть держит, пока поколение играет.
        feed: LoadedFeed,
        /// Что сделать сразу после загрузки, чтобы поколение стартовало где нужно.
        after_load: AfterLoad,
    },
    /// Не открылось: ответ странице по классу ошибки (только класс — в журнал).
    Failed {
        /// Класс ошибки для журнала.
        class: &'static str,
        /// Что ответить странице.
        response: FailureResponse,
    },
    /// Открылось, но плееру не собралось (дефект сборки, а не страницы): видео вернуть.
    Unplayable,
}

impl PendingFeed {
    /// Ждущее поколение из передачи потока приёма.
    pub(super) fn from_source(source: GenerationSource) -> Self {
        match source {
            GenerationSource::Segments { opener, monitor } => Self::Segments {
                opener,
                monitor,
                opened: None,
            },
            GenerationSource::File {
                opener,
                requests,
                prefetch,
            } => Self::File {
                opener,
                requests,
                prefetch,
            },
        }
    }

    /// Это файл `<video src>`.
    pub(super) const fn is_file(&self) -> bool {
        matches!(self, Self::File { .. })
    }

    /// Монитор буфера, если это сегменты (у файла монитора нет).
    pub(super) const fn segments_monitor(&self) -> Option<&PushedStreamMonitor> {
        match self {
            Self::Segments { monitor, .. } => Some(monitor),
            Self::File { .. } => None,
        }
    }

    /// Что ждущее поколение делает с потоком: не открыт, удержан воротами или это файл.
    pub(super) const fn open_state(&self) -> PendingOpenState {
        match self {
            Self::Segments { opened: None, .. } => PendingOpenState::NotOpenedYet,
            Self::Segments {
                opened: Some(_), ..
            } => PendingOpenState::HeldByGate,
            Self::File { .. } => PendingOpenState::NotSegments,
        }
    }

    /// Двигает открытие, не блокируя.
    ///
    /// `placement` решает, когда открытое поколение-сегменты отдать плееру и с какого
    /// места (ворота 04d: с целью — только когда полка её держит); `page` — жива ли
    /// страница, которая может прислать байты у цели. Файл ворот не имеет: его старт
    /// с цели — перемотка после показа.
    pub(super) fn poll_open(
        &mut self,
        placement: &mut GenerationPlacement,
        now: Instant,
        page: PageAvailability,
    ) -> OpenStep {
        match self {
            Self::Segments {
                opener,
                monitor,
                opened,
            } => {
                let stream = match opened.take() {
                    Some(stream) => stream,
                    None => match opener.poll_open() {
                        Ok(OpenProgress::Waiting(hint)) => {
                            return OpenStep::Waiting(hint.retry_after());
                        }
                        Ok(OpenProgress::Ready(stream)) => stream,
                        Err(open_error) => {
                            return OpenStep::Failed {
                                class: stream_class_label(&open_error),
                                response: response_to_stream_failure(&open_error),
                            };
                        }
                    },
                };
                let is_live = stream.live_timeline.is_some();
                let start = if is_positionable(&stream.demuxer, is_live) {
                    match placement.decide(monitor, now, page) {
                        PlacementVerdict::HandOver(start) => start,
                        PlacementVerdict::Wait { recheck_at } => {
                            // Поток открыт, но у цели данных нет: ждёт здесь.
                            *opened = Some(stream);
                            return OpenStep::Waiting(recheck_at.saturating_duration_since(now));
                        }
                    }
                } else {
                    // Эфир и поток без длительности не позиционируются (03e): ждать
                    // данных у цели им незачем — старт там, где лежат байты страницы.
                    placement.named_start()
                };
                // Клик зрителя в эфире исполняется перемоткой после показа (04d). Поток
                // без длительности неперематываем (03b): плеер отверг бы перемотку, и
                // клик, как до 04d, не исполняется — страница к нему уже ушла сама.
                let after_load = if is_live {
                    placement.after_live_load()
                } else {
                    AfterLoad::Nothing
                };
                let timeline = if is_live {
                    SegmentsTimeline::Live
                } else {
                    SegmentsTimeline::OnDemand
                };
                match prepared_segments(stream, start, monitor) {
                    Ok(prepared) => OpenStep::Ready {
                        prepared: Box::new(prepared),
                        feed: LoadedFeed::Segments {
                            monitor: monitor.clone(),
                            downloaded_range: segments_downloaded_range(monitor, timeline),
                        },
                        // Позиция ролика уже внутри `PreparedMedia`; эфиру — перемотка
                        // к клику зрителя после показа.
                        after_load,
                    },
                    Err(prepare_error) => {
                        error!(%prepare_error, "The opened browser stream could not be prepared for the player");
                        OpenStep::Unplayable
                    }
                }
            }
            Self::File {
                opener,
                requests,
                prefetch,
            } => match opener.poll_open() {
                FileOpenProgress::Pending => OpenStep::Waiting(FILE_OPEN_POLL_INTERVAL),
                FileOpenProgress::Ready(opened) => match prepared_file(opened, *prefetch) {
                    Ok(prepared) => OpenStep::Ready {
                        prepared: Box::new(prepared),
                        feed: LoadedFeed::File {
                            requests: requests.clone(),
                        },
                        after_load: placement.named_start().after_file_load(),
                    },
                    Err(prepare_error) => {
                        error!(%prepare_error, "The opened browser file could not be prepared for the player");
                        OpenStep::Unplayable
                    }
                },
                FileOpenProgress::Failed(open_error) => OpenStep::Failed {
                    class: file_class_label(&open_error),
                    response: response_to_file_failure(&open_error),
                },
            },
        }
    }

    /// Запросы байтов, которые надо отправить странице (у сегментов их нет).
    ///
    /// Звать после каждого `poll_open`: первый вызов у файла выдаёт первый запрос.
    pub(super) fn take_range_requests(&self) -> Vec<RangeRequest> {
        match self {
            Self::Segments { .. } => Vec::new(),
            Self::File { requests, .. } => requests.take_requests(),
        }
    }
}

impl LoadedFeed {
    /// Запросы байтов, которые надо отправить странице (чтение вперёд, перемотка,
    /// повтор после сбоя). У сегментов их нет.
    pub(super) fn take_range_requests(&self) -> Vec<RangeRequest> {
        match self {
            Self::Segments { .. } => Vec::new(),
            Self::File { requests } => requests.take_requests(),
        }
    }

    /// Что скачано у поколения — для живого скраба окна. У файла — неизвестно: полки
    /// нет, байты идут по запросам странице.
    pub(super) fn downloaded_range(&self) -> DownloadedRange {
        match self {
            Self::Segments {
                downloaded_range, ..
            } => downloaded_range.clone(),
            Self::File { .. } => DownloadedRange::Unknown,
        }
    }

    /// Что поколение говорит монитору и странице в этом проходе.
    ///
    /// `position` — позиция плеера, если наблюдение показывает именно это поколение.
    /// У файла монитора нет: страница ничего не качает сама, позицию никому не
    /// сообщаем и `buffer_state` не шлём.
    pub(super) fn report(&self, position: Option<Duration>) -> FeedReport {
        match self {
            Self::Segments { monitor, .. } => {
                if let Some(position) = position {
                    monitor.note_playback_position(position);
                }
                FeedReport::Segments {
                    seek_request: monitor.take_page_seek_request(),
                    buffer: monitor.buffer_status(),
                }
            }
            Self::File { .. } => FeedReport::File,
        }
    }
}

/// Что поколение сообщает странице: состояние буфера и просьбу перемотки — только у сегментов.
pub(super) enum FeedReport {
    /// Сегменты: просьба перемотать страницу (если есть) и состояние буфера.
    Segments {
        /// Источнику нужны байты с цели, которой нет на полке.
        seek_request: Option<PageSeekRequest>,
        /// Запас вперёд и «принимаю/полон».
        buffer: BufferStatus,
    },
    /// Файл: странице ничего про буфер не говорим.
    File,
}

/// Запросы байтов в порядке выдачи (по сквозному номеру сеанса).
///
/// Запросы ждущего и загруженного файлов собираются в один список, а номера общие на
/// сеанс: порядок номеров и есть порядок, в котором источники их выдали.
pub(super) fn in_issue_order(
    requests: impl IntoIterator<Item = RangeRequest>,
) -> Vec<RangeRequest> {
    let mut ordered: Vec<RangeRequest> = requests.into_iter().collect();
    ordered.sort_by_key(|request| request.id);
    ordered
}

/// Готовый поток сегментов → медиа для плеера: на стартовой позиции поколения и с
/// живой шкалой, если это эфир.
fn prepared_segments(
    opened: OpenedPushedStream,
    start: GenerationStart,
    monitor: &PushedStreamMonitor,
) -> Result<PreparedMedia, SegmentsPrepareError> {
    let OpenedPushedStream {
        mut demuxer,
        live_timeline,
    } = opened;
    let initial_position =
        position_segments_demuxer(start, monitor, &mut demuxer, live_timeline.is_some());
    let prepared = PreparedMedia::from_external_label(BROWSER_STREAM_LABEL, Box::new(demuxer))
        .with_prepared_initial_position(initial_position)?;
    match live_timeline {
        Some(timeline) => Ok(prepared.with_dynamic_timeline(timeline)?),
        None => Ok(prepared),
    }
}

/// Почему готовый поток сегментов не стал медиа для плеера. Оба случая — дефект
/// договора с источником, а не данных страницы.
#[derive(Debug, thiserror::Error)]
enum SegmentsPrepareError {
    /// Источник отдаёт живую шкалу только потоку без длительности.
    #[error("live timeline does not fit the browser stream: {0}")]
    LiveTimeline(#[from] PreparedMediaTimelineModeError),
    /// Итог перемотки источника противоречит стартовой позиции.
    #[error("start position does not fit the browser stream: {0}")]
    StartPosition(#[from] PreparedInitialPositionError),
}

/// Почему открытый файл не стал медиа для плеера.
#[derive(Debug, thiserror::Error)]
pub(super) enum FilePrepareError {
    /// Фоновый поток демуксера не поднялся.
    #[error("the background demuxer for the file did not start: {0}")]
    BackgroundDemuxer(String),
    /// Сборка `PreparedMedia` не прошла.
    #[error("the media for the file could not be composed: {0}")]
    Compose(String),
}

/// Открытый файл → медиа для плеера, как у обычной progressive-ссылки.
///
/// Демуксер файла блокирующий (читает через окно предзагрузки и ждёт байтов страницы),
/// поэтому он уходит в фоновый поток тем же путём, что direct-ссылка: плеер получает
/// неблокирующий демуксер и, если размер файла известен, порт перемотки по запросам
/// байтов. Файл без известного размера идёт «вперёд» без перемотки (решение владельца).
fn prepared_file(
    opened: OpenedPushedFile,
    prefetch: PrefetchConfig,
) -> Result<PreparedMedia, FilePrepareError> {
    let transport = match opened.seekability {
        FileSeekability::Seekable => TransportSeekability::Seekable,
        FileSeekability::ForwardOnly => TransportSeekability::Streaming,
    };
    let player_demuxer = into_player_demuxer(
        opened.demuxer,
        transport,
        opened.lifecycle_cancellation,
        prefetch,
    )
    .map_err(|error| FilePrepareError::BackgroundDemuxer(format!("{error:#}")))?;
    compose_prepared_web_media(
        BROWSER_STREAM_LABEL,
        player_demuxer.demuxer,
        PreparedWebMediaAttachments {
            demux_seek: player_demuxer
                .seek_port
                .map(PreparedWebMediaSeekAttachment::WorkerReceipted),
            ..PreparedWebMediaAttachments::default()
        },
    )
    .map_err(|error| FilePrepareError::Compose(error.to_string()))
}

/// Класс ошибки потока словом для журнала.
fn stream_class_label(error: &pushed_media_source::PushedStreamError) -> &'static str {
    use pushed_media_source::PushedStreamFailureClass as Class;
    match error.failure_class() {
        Class::ProtectedContent => "protected_content",
        Class::UnsupportedMedia => "unsupported_media",
        Class::UnparsableMedia => "unparsable_media",
        Class::InputMisuse => "input_misuse",
    }
}

/// Класс ошибки файла словом для журнала.
fn file_class_label(error: &pushed_media_source::PushedFileError) -> &'static str {
    use pushed_media_source::PushedFileFailureClass as Class;
    match error.failure_class() {
        Class::ProtectedContent => "protected_content",
        Class::UnsupportedMedia => "unsupported_media",
        Class::UnparsableMedia => "unparsable_media",
        Class::InputMisuse => "input_misuse",
        Class::SourceUnavailable => "source_unavailable",
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use pushed_media_source::RangeRequestId;

    use super::*;

    fn request(id: u32, offset: u64) -> RangeRequest {
        RangeRequest {
            id: RangeRequestId::new(id),
            offset,
            length: NonZeroU32::new(10).expect("non-zero"),
        }
    }

    /// Запросы ждущего и загруженного файлов уходят странице по сквозным номерам, а не
    /// «сначала ждущего, потом загруженного»: загруженный мог выдать свой запрос позже.
    #[test]
    fn requests_of_two_files_go_out_in_issue_order() {
        let pending = vec![request(5, 0), request(7, 70)];
        let loaded = vec![request(6, 60)];

        let ordered = in_issue_order(pending.into_iter().chain(loaded));

        let ids: Vec<u32> = ordered.iter().map(|request| request.id.get()).collect();
        assert_eq!(ids, vec![5, 6, 7]);
        assert_eq!(ordered[1].offset, 60, "requests are not rewritten");
    }

    #[test]
    fn no_requests_stay_no_requests() {
        assert!(in_issue_order(Vec::new()).is_empty());
    }
}
