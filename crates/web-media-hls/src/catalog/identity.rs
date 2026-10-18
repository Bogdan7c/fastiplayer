//! Построение компактных opaque-ключей строк HLS-каталога.
//!
//! Ключи живут только внутри процесса: `ComponentVariantExactKey` /
//! `ComponentVariantSemanticKey` не реализуют serde, не имеют raw-аксессора и не
//! попадают в конфиг или durable state. Через refresh/reopen их сопоставляет
//! `semantic_rematch` в памяти, сравнивая ключи, построенные одним и тем же кодом
//! из свежего манифеста. Поэтому формат ключа можно менять без миграции.

use sha2::{Digest, Sha256};
use web_media_core::{AudioTrackDescriptor, VideoTrackDescriptor};

/// Первичный порядок строк каталога, зашитый в начало ключа.
///
/// `build::sort_rows` сортирует строки по строковому ключу ради детерминизма, а UI/индексы
/// наблюдают этот порядок (ladder по возрастанию bandwidth). Хэш сам по себе порядок
/// перемешивает, поэтому осмысленный порядок выносится в явное фиксированное поле ключа.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct KeyOrdering(u64);

impl KeyOrdering {
    /// Порядок по возрастанию `BANDWIDTH` варианта (как раньше задавал первый framed-field).
    pub(super) const fn by_bandwidth(bandwidth: u64) -> Self {
        Self(bandwidth)
    }

    /// Нет осмысленного первичного порядка: строки упорядочиваются только хэшем.
    pub(super) const fn unordered() -> Self {
        Self(0)
    }
}

/// Строитель ключа вида `<prefix>:<16 hex ordering>:<64 hex SHA-256>`.
///
/// Поля манифеста подаются в хэш с length-framing (`u64` big-endian длина + байты),
/// поэтому `("ab","c")` и `("a","bc")` дают разные ключи. Итоговая длина ключа
/// `prefix.len() + 1 + 16 + 1 + 64` не зависит от содержимого манифеста, а значит всегда
/// укладывается в `MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES` (prefix — короткая
/// статическая константа каталога), и построение ключа не может завершиться ошибкой.
pub(super) struct SemanticKeyBuilder {
    /// Статический префикс типа строки (`hls-v1-coupled` и т.п.).
    prefix: &'static str,
    /// Первичный порядок строки (идёт в ключ открытым текстом фиксированной ширины).
    ordering: KeyOrdering,
    /// Накопитель хэша всех framed-полей.
    hasher: Sha256,
}

impl SemanticKeyBuilder {
    /// Начинает ключ; префикс входит и в хэш (domain separation между типами строк).
    pub(super) fn new(prefix: &'static str, ordering: KeyOrdering) -> Self {
        let mut builder = Self {
            prefix,
            ordering,
            hasher: Sha256::new(),
        };
        builder.field(prefix.as_bytes());
        builder
    }

    /// Добавляет поле: сначала длина (8 байт big-endian), затем сами байты.
    pub(super) fn field(&mut self, value: &[u8]) {
        // `usize` -> `u64` без потерь на всех поддерживаемых платформах; `try_from`
        // не нужен, но saturating исключает панику на гипотетической 128-битной цели.
        let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
        self.hasher.update(length.to_be_bytes());
        self.hasher.update(value);
    }

    pub(super) fn optional_field(&mut self, value: Option<&[u8]>) {
        match value {
            Some(value) => {
                self.field(b"some");
                self.field(value);
            }
            None => self.field(b"none"),
        }
    }

    pub(super) fn video(&mut self, video: &VideoTrackDescriptor) {
        self.field(video.codec().raw().as_str().as_bytes());
        let width = video.width_pixels().map(u32::to_be_bytes);
        self.optional_field(width.as_ref().map(|value| value.as_slice()));
        let height = video.height().map(|height| height.pixels().to_be_bytes());
        self.optional_field(height.as_ref().map(|value| value.as_slice()));
        let frame_rate = video.frame_rate().map(|rate| {
            let mut bytes = [0_u8; 8];
            bytes[..4].copy_from_slice(&rate.numerator().to_be_bytes());
            bytes[4..].copy_from_slice(&rate.denominator().to_be_bytes());
            bytes
        });
        self.optional_field(frame_rate.as_ref().map(|value| value.as_slice()));
        let bitrate = video
            .bitrate()
            .map(|rate| rate.bits_per_second().to_be_bytes());
        self.optional_field(bitrate.as_ref().map(|value| value.as_slice()));
        self.field(match video.dynamic_range() {
            web_media_core::DynamicRange::Sdr => b"sdr",
            web_media_core::DynamicRange::Hdr => b"hdr",
            web_media_core::DynamicRange::Unknown => b"unknown",
        });
    }

    pub(super) fn audio(&mut self, audio: &AudioTrackDescriptor) {
        self.field(audio.codec().raw().as_str().as_bytes());
        let sample_rate = audio.sample_rate().map(|rate| rate.hertz().to_be_bytes());
        self.optional_field(sample_rate.as_ref().map(|value| value.as_slice()));
        let channels = audio
            .channels()
            .map(|channels| channels.get().to_be_bytes());
        self.optional_field(channels.as_ref().map(|value| value.as_slice()));
        let bitrate = audio
            .bitrate()
            .map(|rate| rate.bits_per_second().to_be_bytes());
        self.optional_field(bitrate.as_ref().map(|value| value.as_slice()));
        self.optional_field(
            audio
                .language()
                .map(|language| language.as_str().as_bytes()),
        );
    }

    /// Завершает ключ: `<prefix>:<ordering>:<hex SHA-256>`, фиксированной длины.
    pub(super) fn finish(self) -> String {
        let digest = self.hasher.finalize();
        let mut key = String::with_capacity(self.prefix.len() + 2 + 16 + 2 * digest.len());
        key.push_str(self.prefix);
        key.push(':');
        // Фиксированная ширина 16 hex-символов: лексикографический порядок = числовой.
        key.push_str(&format!("{:016x}", self.ordering.0));
        key.push(':');
        key.push_str(
            &digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        );
        key
    }
}

#[cfg(test)]
mod tests {
    use web_media_core::MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES;

    use super::{KeyOrdering, SemanticKeyBuilder};

    /// Все префиксы строк каталога (см. `rows.rs`); длина ключа обязана укладываться в лимит.
    const ROW_PREFIXES: [&str; 4] = [
        "hls-v1-video",
        "hls-v1-audio-variant",
        "hls-v1-audio-rendition",
        "hls-v1-coupled",
    ];

    fn key_of(prefix: &'static str, fields: &[&[u8]]) -> String {
        let mut builder = SemanticKeyBuilder::new(prefix, KeyOrdering::unordered());
        for field in fields {
            builder.field(field);
        }
        builder.finish()
    }

    #[test]
    fn key_length_is_fixed_regardless_of_field_content() {
        let huge = vec![0xAB_u8; 10_000];
        for prefix in ROW_PREFIXES {
            let small = key_of(prefix, &[b"x"]);
            let large = key_of(prefix, &[&huge, &huge, &huge]);
            assert_eq!(small.len(), prefix.len() + 1 + 16 + 1 + 64);
            assert_eq!(large.len(), small.len());
            assert!(large.len() <= MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES);
            assert!(large.starts_with(prefix));
        }
    }

    #[test]
    fn same_fields_give_same_key_and_framing_keeps_boundaries_distinct() {
        assert_eq!(
            key_of("hls-v1-coupled", &[b"ab", b"c"]),
            key_of("hls-v1-coupled", &[b"ab", b"c"])
        );
        assert_ne!(
            key_of("hls-v1-coupled", &[b"ab", b"c"]),
            key_of("hls-v1-coupled", &[b"a", b"bc"])
        );
        assert_ne!(
            key_of("hls-v1-coupled", &[b"a"]),
            key_of("hls-v1-coupled", &[b"a", b""])
        );
        assert_ne!(
            key_of("hls-v1-coupled", &[b"a"]),
            key_of("hls-v1-video", &[b"a"])
        );
    }

    #[test]
    fn optional_field_distinguishes_absent_from_present_empty() {
        let mut absent = SemanticKeyBuilder::new("hls-v1-coupled", KeyOrdering::unordered());
        absent.optional_field(None);
        let mut empty = SemanticKeyBuilder::new("hls-v1-coupled", KeyOrdering::unordered());
        empty.optional_field(Some(b""));
        assert_ne!(absent.finish(), empty.finish());
    }

    #[test]
    fn bandwidth_ordering_survives_string_sort_of_keys() {
        let key_at = |bandwidth: u64| {
            let mut builder =
                SemanticKeyBuilder::new("hls-v1-coupled", KeyOrdering::by_bandwidth(bandwidth));
            builder.field(&bandwidth.to_be_bytes());
            builder.finish()
        };
        let mut keys = vec![
            key_at(9_000_000),
            key_at(300),
            key_at(3_849_487),
            key_at(u64::MAX),
        ];
        keys.sort();
        assert_eq!(
            keys,
            vec![
                key_at(300),
                key_at(3_849_487),
                key_at(9_000_000),
                key_at(u64::MAX)
            ]
        );
    }
}
