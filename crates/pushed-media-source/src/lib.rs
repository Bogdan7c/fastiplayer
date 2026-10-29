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
//! Страница — враг: размеры сегментов ограничены, шифрование отвергается, любая
//! ошибка — ошибка одного потока, а не процесса.

mod container;
mod episode;
mod error;
mod framing;
mod intake;
mod lane;
mod lane_demuxer;
mod pipe;
mod policy;
mod protection;
mod random_access;
mod stream;
mod stream_state;
mod time;
mod track_identity;

pub use container::{PushedContainer, UnsupportedMediaType};
pub use error::{LaneLayoutProblem, PushedStreamError, PushedStreamFailureClass};
pub use framing::{FramingError, SegmentKind};
pub use lane::{AppendOutcome, LaneId};
pub use policy::{PushedSourcePolicy, PushedSourcePolicyError};
pub use stream::{
    EndOfStreamReport, OpenProgress, PushedStreamDemuxer, PushedStreamOpener,
    PushedStreamStartError, PushedStreamWriter, pushed_stream,
};
pub use time::{PushedDuration, TimestampOffset};
