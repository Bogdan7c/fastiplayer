//! Источник медиа «сегменты извне»: байты приносит кто-то другой, мы только ждём.
//!
//! Первый потребитель — браузерный режим Fastiplayer (мост «браузер качает — мы
//! играем», issue #2): экстеншен перехватывает куски, которые плеер страницы кладёт
//! в MSE `SourceBuffer`, и шлёт их сюда. Сам crate о браузере ничего не знает:
//! «полоса» — это SourceBuffer, «поток» — один `MediaSource` (поколение).
//!
//! Главный механизм («сортировщик», подтверждён владельцем, решение 90):
//! 1. куски каждой полосы склеиваются в целые сегменты (модуль `framing`);
//! 2. сегменты кормят существующие демуксеры (`symphonia-demux`, `mpeg-ts-demux`)
//!    в фоновых потоках (`ProgressiveDemuxer`);
//! 3. новый init-сегмент, смена контейнера или смещения времени начинают новый
//!    «эпизод» — свежий демуксер, как у живых HLS и DASH;
//! 4. плеер получает один [`Demuxer`](media_core::Demuxer): стабильные ID дорожек,
//!    `TracksChanged` только при настоящей смене, `TemporarilyUnavailable`, пока
//!    байтов нет.
//!
//! «Полка» (сессия 03b, подтверждена владельцем): целые медиасегменты fMP4 и WebM
//! размечаются по времени и ложатся на полку полосы по месту во времени, как
//! `buffered` в MSE. Демуксеру подаётся только то, что продолжает уже поданное
//! (курсор подачи); пришедшее раньше времени ждёт дыру, повтор прошлого не
//! играется дважды. Полка помнит прошлое, пока его помнит страница: уходит оно,
//! когда страница сама стёрла его ([`PushedStreamWriter::note_page_removed`]), или
//! под предохранителем памяти у порога «полон». Окно ([`BufferWindow`]) и монитор
//! ([`PushedStreamMonitor`]) дают странице «буферизовано» и «полон». MPEG-TS
//! время не размечает и играет по-старому, в порядке прихода.
//!
//! Перемотка (сессия 03b, пакет F): цель на полке — подача начинается заново с
//! сегмента с целью, страницу не трогаем; цели нет — источник просит страницу
//! перемотаться ([`PushedStreamMonitor::take_page_seek_request`]) и ждёт её байтов
//! с цели; перемотку, начатую самой страницей, приложение отмечает
//! ([`PushedStreamWriter::note_page_seek`]). Живой эфир получает динамическую шкалу
//! по полке ([`OpenedPushedStream::live_timeline`]).
//!
//! Пределы (пакет G 03b): «полон» приходит и по байтам (доля жёсткого потолка),
//! честная страница до потолка не доходит; предел ждущих эпизодов не убивает
//! поток — подача ждёт на полке; повтор уже выданного времени после смены эпизода
//! плееру не уходит (модуль `episode_seam`); конец кластера WebM без длительности
//! кадра оценивается (`framing::SegmentEnd`).
//!
//! Файл по запросам байтов (сессия 03c): обычный MP4 или WebM, который мы не качаем,
//! — каждый диапазон просим у внешней стороны ([`pushed_file`]), для плеера это
//! обычный файл с перемоткой, как progressive HTTP.
//!
//! Страница — враг: размеры сегментов ограничены, шифрование отвергается, любая
//! ошибка — ошибка одного потока, а не процесса.

mod buffer_window;
mod container;
mod episode;
mod episode_seam;
mod error;
mod feed;
mod framing;
mod intake;
mod lane;
mod lane_demuxer;
mod live_timeline;
mod mailbox;
mod monitor;
mod page_removal;
mod page_seek;
mod pipe;
mod policy;
mod protection;
mod random_access;
mod range_file;
mod shelf;
mod stream;
mod stream_buffer;
mod stream_seek;
mod stream_state;
#[cfg(test)]
mod test_fixtures;
mod time;
mod track_identity;

pub use buffer_window::{BufferIntake, BufferStatus, BufferWindow};
pub use container::{PushedContainer, UnsupportedMediaType};
pub use error::{LaneLayoutProblem, PushedStreamError, PushedStreamFailureClass};
pub use framing::{FramingError, SegmentKind};
pub use lane::{AppendOutcome, LaneId};
pub use monitor::{PushedStreamMonitor, ShelfSeekOutlook};
pub use page_removal::{
    PageRemovalOutcome, PageRemovalReport, PageRemovedRange, PageRemovedRangeEnd,
};
pub use page_seek::PageSeekRequest;
pub use policy::{PushedSourcePolicy, PushedSourcePolicyError};
pub use range_file::{
    FileOpenProgress, FileSeekability, MAX_RANGE_REQUEST_BYTES, OpenedPushedFile, PushedFileError,
    PushedFileFailureClass, PushedFileOpener, PushedFilePolicy, PushedFilePolicyError,
    PushedFileRequests, PushedFileWriter, RangeBytesVerdict, RangeFailure, RangeIdStanding,
    RangeOutcome, RangeRequest, RangeRequestId, RangeRequestIds, RangeRequestNotifier,
    RangeRetryPolicy, ResourceSizeReport, UnsupportedFileLayout, pushed_file,
};
pub use stream::{
    EndOfStreamReport, OpenProgress, OpenedPushedStream, PushedStreamDemuxer, PushedStreamOpener,
    PushedStreamStartError, PushedStreamWriter, pushed_stream,
};
pub use time::{PushedDuration, TimestampOffset};
