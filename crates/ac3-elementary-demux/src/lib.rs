//! Demuxer «сырых» AC-3 / E-AC-3 файлов (`.ac3`, `.eac3`, `.ec3`).
//!
//! Такой файл — это просто подряд идущие syncframe-ы без контейнера: ни
//! индекса, ни таблицы длительности, ни timestamp-ов. Поэтому crate сам
//! отвечает за то, что в других форматах даёт контейнер:
//!
//! - **распознавание по содержимому** (`probe`): синхрослово `0x0B77` в начале
//!   (после ID3v2-тегов) и цепочка корректных кадров подряд;
//! - **нарезку на пакеты** (`framing`): границы кадров и access unit-ов
//!   (E-AC-3: независимый substream 0 + его dependent-ы, только основная
//!   программа) целиком берутся из `ac3-syncframe`;
//! - **время** (`timeline`): PTS = накопленное число сэмплов; длительность и
//!   seek — по оценке «байт на сэмпл», измеренной на первых кадрах файла
//!   (точно для потоков с постоянной длиной кадра);
//! - **устойчивость**: мусор/обрыв в начале, потеря синхронизации посреди
//!   файла (bounded resync) и оборванный хвост не теряют корректных кадров.
//!
//! Декодирование — забота `audio` (codec id `A_AC3` / `A_EAC3`, как у
//! MKV/MP4/MPEG-TS путей).

#![forbid(unsafe_code)]

mod demuxer;
mod error;
mod factory;
mod framing;
mod leading_tags;
mod options;
mod probe;
mod reader;
mod timeline;

pub use demuxer::Ac3ElementaryDemuxer;
pub use error::{Ac3ElementaryDemuxError, Ac3ElementaryOptionsError};
pub use factory::{AC3_ELEMENTARY_CONTAINER_ID, Ac3ElementaryDemuxFactory};
pub use options::{Ac3ElementaryDemuxOptions, Ac3ElementaryLimit};
