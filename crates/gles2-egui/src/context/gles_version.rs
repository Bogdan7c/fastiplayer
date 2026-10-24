//! Разбор строки `GL_VERSION` контекста OpenGL ES.
//!
//! Правило намеренно повторяет разбор внутри `glow` 0.17 (`Version::parse`): берётся текст после
//! ПОСЛЕДНЕГО вхождения `" ES "`, затем `<major>.<minor>` до первого пробела. Если наш разбор
//! успешен, конструктор glow не упадёт на этой строке (`safety::load_gl_for_current_context`).
//! Дополнительно требуем префикс `OpenGL ES ` (GLES 2.0, раздел 6.1.5): desktop GL и ES-CM 1.x
//! не годятся.

/// Версия контекста OpenGL ES, как её сообщил драйвер.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlesVersion {
    /// Старшая часть (у radeonsi и llvmpipe на запрос ES 2 — `3`).
    pub major: u32,
    /// Младшая часть.
    pub minor: u32,
}

impl GlesVersion {
    /// OpenGL ES 2.0 — минимум и одновременно потолок того, что мы используем.
    pub const ES_2_0: Self = Self { major: 2, minor: 0 };
}

/// Разбирает `GL_VERSION`. `None` — строка не от контекста OpenGL ES 2.0+ или её не разобрал бы
/// glow.
pub(crate) fn parse_like_glow(version_string: &str) -> Option<GlesVersion> {
    // Спецификация: «OpenGL ES N.M <vendor-specific information>».
    if !version_string.starts_with("OpenGL ES ") {
        return None;
    }
    // glow: всё после последнего " ES ".
    let (_, after_es) = version_string.rsplit_once(" ES ")?;
    // glow: номер до первого пробела.
    let number = after_es.split(' ').next()?;
    let mut parts = number.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    // glow: младшая часть целиком, с его же нормализацией нулей («0…» → 0, «20» → 2).
    // Хвост после ведущего нуля glow отбрасывает («0-rc1» → 0), а «1x» не разбирает вовсе —
    // тогда и мы обязаны отказать.
    let minor_text = parts.next()?;
    let normalized_minor = if minor_text.starts_with('0') {
        "0"
    } else {
        minor_text.trim_end_matches('0')
    };
    let minor = normalized_minor.parse::<u32>().ok()?;
    let version = GlesVersion { major, minor };
    (version >= GlesVersion::ES_2_0).then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ironlake_and_mesa_es_strings_parse() {
        // Живые строки разведки 00: crocus на T410 и radeonsi на 780M (запрос ES 2 → ES 3.2).
        assert_eq!(
            parse_like_glow("OpenGL ES 2.0 Mesa 26.2.4"),
            Some(GlesVersion { major: 2, minor: 0 })
        );
        assert_eq!(
            parse_like_glow("OpenGL ES 3.2 Mesa 26.2.5-arch3.1"),
            Some(GlesVersion { major: 3, minor: 2 })
        );
    }

    #[test]
    fn non_es_and_too_old_contexts_are_rejected() {
        assert_eq!(parse_like_glow("4.6 (Core Profile) Mesa 26.2.5"), None);
        assert_eq!(parse_like_glow("OpenGL ES-CM 1.1 Mesa"), None);
        assert_eq!(parse_like_glow("OpenGL ES 1.1 Mesa"), None);
        assert_eq!(parse_like_glow(""), None);
    }

    #[test]
    fn last_es_marker_wins_like_glow() {
        // Вендорская часть со своим « ES » сбивает glow — значит, и мы обязаны отказать,
        // иначе конструктор glow запаникует.
        assert_eq!(parse_like_glow("OpenGL ES 2.0 Vendor ES build"), None);
    }

    #[test]
    fn minor_part_is_normalized_exactly_like_glow() {
        assert_eq!(
            parse_like_glow("OpenGL ES 3.20 Vendor"),
            Some(GlesVersion { major: 3, minor: 2 })
        );
        assert_eq!(
            parse_like_glow("OpenGL ES 2.0-rc1 Vendor"),
            Some(GlesVersion { major: 2, minor: 0 })
        );
        assert_eq!(parse_like_glow("OpenGL ES 2.1x Vendor"), None);
    }
}
