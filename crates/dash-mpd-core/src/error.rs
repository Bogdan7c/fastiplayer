use bounded_xml_reader::XmlReadError;

/// Stable категория отказа без входных XML/URL значений.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashMpdErrorKind {
    /// Hardened XML boundary отверг документ.
    Xml,
    /// Корень или namespace не соответствуют DASH MPD.
    InvalidRoot,
    /// Live/dynamic MPD не входит в S34 VOD.
    DynamicPresentation,
    /// Declared DASH profile не входит в доказанный S34 static allowlist.
    UnsupportedProfile,
    /// Обязательное поле отсутствует или имеет неверную форму.
    InvalidAttribute,
    /// Неизвестная playback-changing конструкция не поддержана.
    UnsupportedConstruct,
    /// DRM/ContentProtection не поддерживается.
    ContentProtection,
    /// На одном уровне задано больше одного BaseURL.
    MultipleBaseUrls,
    /// Safety cap модели исчерпан.
    LimitExceeded,
    /// Адресация Representation неоднозначна или неполна.
    InvalidAddressing,
    /// Container/codec/content evidence противоречат друг другу.
    UnsupportedMediaEvidence,
    /// Периоды не образуют конечный непрерывный VOD timeline.
    InvalidPeriodTimeline,
    /// XML оборван или нарушает вложенность поддерживаемой схемы.
    MalformedSchema,
}

/// Ошибка MPD parser-а намеренно не хранит raw XML, атрибуты или URL.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("DASH MPD rejected: {kind:?}")]
pub struct DashMpdError {
    /// Машиночитаемая стабильная категория.
    kind: DashMpdErrorKind,
    /// Exact hardened XML reason сохраняется без parser-specific quick-xml types.
    #[source]
    xml_error: Option<XmlReadError>,
}

impl DashMpdError {
    /// Создаёт secret-safe ошибку внутри schema owner-а.
    pub(crate) const fn new(kind: DashMpdErrorKind) -> Self {
        Self {
            kind,
            xml_error: None,
        }
    }

    /// Возвращает категорию для policy/UI mapping.
    pub const fn kind(&self) -> DashMpdErrorKind {
        self.kind
    }

    /// Возвращает exact S04X reason для budget/security/malformed distinction.
    pub const fn xml_error(&self) -> Option<&XmlReadError> {
        self.xml_error.as_ref()
    }

    /// Сворачивает parser-specific XML ошибку в безопасную boundary-категорию.
    pub(crate) fn from_xml(error: XmlReadError) -> Self {
        Self {
            kind: DashMpdErrorKind::Xml,
            xml_error: Some(error),
        }
    }
}
