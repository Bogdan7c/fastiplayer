//! ID3v2-теги в начале «сырого» AC-3 файла.
//!
//! Контейнера у elementary stream-а нет, но теггеры (как и для MP3) часто
//! дописывают в начало ID3v2 — с названием и обложкой. Внутри тега могут
//! встретиться байты `0x0B 0x77`, поэтому тег пропускается целиком по
//! объявленному размеру, а не «поиском синхрослова сквозь него».

/// Заголовок ID3v2: `"ID3"`, версия (2 байта), флаги, synchsafe-размер (4 байта).
pub(crate) const ID3V2_HEADER_BYTES: usize = 10;

/// Подпись тега.
const ID3V2_MAGIC: &[u8; 3] = b"ID3";

/// Флаг ID3v2.4 «после тела есть footer» (ещё 10 байт).
const ID3V2_FOOTER_PRESENT_FLAG: u8 = 0x10;

/// Сколько тегов подряд пропускаем; больше в настоящих файлах не бывает,
/// а предел не даёт зациклиться на поддельной цепочке.
pub(crate) const MAX_LEADING_TAGS: usize = 4;

/// Что лежит в начале буфера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeadingTag {
    /// Полный заголовок ID3v2; тег вместе с заголовком (и footer-ом) занимает `total_bytes`.
    Id3v2 {
        /// Размер всего тега в байтах.
        total_bytes: u64,
    },
    /// Похоже на начало ID3v2, но заголовок ещё не дочитан.
    NeedMoreData,
    /// Тега нет (или байты лишь похожи на него, но заголовок некорректен).
    Absent,
}

/// Классифицирует начало буфера, не требуя, чтобы тело тега было дочитано.
pub(crate) fn classify_leading_tag(bytes: &[u8]) -> LeadingTag {
    let compared = bytes.len().min(ID3V2_MAGIC.len());
    if bytes[..compared] != ID3V2_MAGIC[..compared] {
        return LeadingTag::Absent;
    }
    let Some(header) = bytes.first_chunk::<ID3V2_HEADER_BYTES>() else {
        return LeadingTag::NeedMoreData;
    };
    let [_, _, _, major_version, revision, flags, size @ ..] = *header;
    // Версия и ревизия 0xFF запрещены спецификацией; каждый байт размера
    // synchsafe (старший бит 0). Иначе это не ID3v2, а случайные байты.
    if major_version == 0xFF || revision == 0xFF || size.iter().any(|byte| byte & 0x80 != 0) {
        return LeadingTag::Absent;
    }
    let body_bytes = size.iter().fold(0_u64, |accumulated, byte| {
        (accumulated << 7) | u64::from(*byte)
    });
    let footer_bytes = if flags & ID3V2_FOOTER_PRESENT_FLAG != 0 {
        ID3V2_HEADER_BYTES as u64
    } else {
        0
    };
    LeadingTag::Id3v2 {
        total_bytes: ID3V2_HEADER_BYTES as u64 + body_bytes + footer_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::{LeadingTag, classify_leading_tag};

    /// Synchsafe-размер 0x00 0x00 0x02 0x01 = 2·128 + 1 = 257 байт тела.
    #[test]
    fn id3v2_size_is_synchsafe_and_includes_header() {
        let header = *b"ID3\x04\x00\x00\x00\x00\x02\x01";
        assert_eq!(
            classify_leading_tag(&header),
            LeadingTag::Id3v2 { total_bytes: 267 }
        );
    }

    #[test]
    fn id3v24_footer_adds_ten_bytes() {
        let header = *b"ID3\x04\x00\x10\x00\x00\x00\x05";
        assert_eq!(
            classify_leading_tag(&header),
            LeadingTag::Id3v2 { total_bytes: 25 }
        );
    }

    #[test]
    fn partial_magic_waits_and_other_bytes_are_absent() {
        assert_eq!(classify_leading_tag(b"ID"), LeadingTag::NeedMoreData);
        assert_eq!(
            classify_leading_tag(b"ID3\x04\x00"),
            LeadingTag::NeedMoreData
        );
        assert_eq!(
            classify_leading_tag(&[0x0B, 0x77, 0, 0]),
            LeadingTag::Absent
        );
        assert_eq!(classify_leading_tag(&[]), LeadingTag::NeedMoreData);
    }

    /// Старший бит в байте размера — не synchsafe, значит это не ID3v2.
    #[test]
    fn non_synchsafe_size_is_not_a_tag() {
        let header = *b"ID3\x04\x00\x00\x80\x00\x00\x00";
        assert_eq!(classify_leading_tag(&header), LeadingTag::Absent);
    }
}
