//! Декодер субтитров Blu-ray PGS (`S_HDMV/PGS`): сегменты -> композиции -> картинки.
//!
//! Слои crate:
//! 1. [`segment`] — нарезка блока Matroska / файла `.sup` на сегменты;
//!    [`pcs`], [`wds`], [`pds`], [`ods`] — разбор сегментов;
//! 2. [`color`], [`rle`], [`picture_decode`] — палитра YCbCr, RLE, пиксели RGBA;
//! 3. [`decoder`] — автомат дорожки: наборы отображения -> композиции с растровыми репликами;
//! 4. [`store`] — хранилище композиций с интервалами показа и политикой памяти;
//! 5. [`stream_builder`] — кодировщик тестовых потоков (для тестов и генерации фикстур).
//!
//! Crate нейтральный: зависит только от `subtitle-core`; ни GPU, ни UI, ни контейнера.
//! Реализован по публичному описанию формата; код других декодеров не использовался.

pub mod color;
pub mod decoder;
pub mod error;
pub mod ods;
pub mod pcs;
pub mod pds;
pub mod picture_decode;
mod reader;
pub mod rle;
pub mod segment;
pub mod store;
#[cfg(any(test, feature = "test-support"))]
pub mod stream_builder;
pub mod wds;

pub use color::{PgsColorMatrix, ycbcr_to_rgb};
pub use decoder::{PgsComposition, PgsDecodeStatistics, PgsDecoderLimits, PgsTrackDecoder};
pub use error::SegmentParseError;
pub use picture_decode::{
    DecodedPicture, PictureDecodeError, PictureDecodeLimits, PremultipliedRgbaImage,
    decode_picture_rgba,
};
pub use rle::{DecodedIndices, RleDataEnd, RleReport, decode_hdmv_rle};
pub use segment::{
    Segment, SegmentFramingError, SegmentKind, SplitBlock, SupError, SupSegment, read_sup,
    split_block, ticks_90khz_to_time,
};
pub use store::{
    CompositionEnd, CompositionHandle, DiscontinuityOutcome, PgsCompositionStore, PgsRetention,
    PgsStoreStatistics, PredecessorClosed, RetentionOutcome, StoreChange, TailAppend,
};
