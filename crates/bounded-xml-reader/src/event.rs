//! Parser-neutral XML events для domain schema owners.

/// Expanded XML name хранит namespace URI и local name без зависимости от prefix-а.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct XmlExpandedName {
    /// `None` означает отсутствие namespace binding, а не ошибку resolver-а.
    namespace_uri: Option<String>,
    /// Local name не содержит namespace prefix.
    local_name: String,
}

impl XmlExpandedName {
    /// Создаётся только reader-ом после успешного namespace resolution и UTF-8 decode.
    pub(crate) fn new(namespace_uri: Option<String>, local_name: String) -> Self {
        Self {
            namespace_uri,
            local_name,
        }
    }

    /// Возвращает resolved namespace URI, если name находится в namespace.
    pub fn namespace_uri(&self) -> Option<&str> {
        self.namespace_uri.as_deref()
    }

    /// Возвращает local name без prefix-а.
    pub fn local_name(&self) -> &str {
        &self.local_name
    }
}

/// Один materialized non-namespace XML attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlAttribute {
    /// Имя разрешено по attribute namespace rules: default namespace не применяется.
    name: XmlExpandedName,
    /// Значение нормализовано и раскрывает только numeric/predefined references.
    value: String,
}

impl XmlAttribute {
    /// Reader создаёт attribute после всех budget и entity checks.
    pub(crate) fn new(name: XmlExpandedName, value: String) -> Self {
        Self { name, value }
    }

    /// Возвращает namespace-resolved attribute name.
    pub fn name(&self) -> &XmlExpandedName {
        &self.name
    }

    /// Возвращает decoded и XML-normalized attribute value.
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// Start/empty element с уже проверенными attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlElement {
    /// Element name использует default namespace согласно XML Namespaces.
    name: XmlExpandedName,
    /// Namespace declarations сюда не попадают: они уже применены boundary.
    attributes: Vec<XmlAttribute>,
}

impl XmlElement {
    /// Reader собирает element только после полного budget accounting start tag-а.
    pub(crate) fn new(name: XmlExpandedName, attributes: Vec<XmlAttribute>) -> Self {
        Self { name, attributes }
    }

    /// Возвращает namespace-resolved element name.
    pub fn name(&self) -> &XmlExpandedName {
        &self.name
    }

    /// Возвращает ordered non-namespace attributes текущего element.
    pub fn attributes(&self) -> &[XmlAttribute] {
        &self.attributes
    }
}

/// Text chunk объединяет обычный text, CDATA или одну legal entity reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlText {
    /// Content уже decoded по XML version и учтён text budget-ом.
    content: String,
}

impl XmlText {
    /// Reader создаёт text только после security validation.
    pub(crate) fn new(content: String) -> Self {
        Self { content }
    }

    /// Возвращает decoded character content.
    pub fn content(&self) -> &str {
        &self.content
    }
}

/// Минимальный XML infoset stream без comments/DTD/parser-specific details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlEvent {
    /// Обычный opening element увеличивает reader depth до matching end.
    StartElement(XmlElement),
    /// Empty element представляет `<name/>` одним domain event-ом.
    EmptyElement(XmlElement),
    /// Closing element содержит то же expanded name vocabulary.
    EndElement(XmlExpandedName),
    /// Text может приходить несколькими chunks, которые domain owner объединяет по schema.
    Text(XmlText),
}
