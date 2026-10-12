//! Stable typed failures hardened XML boundary.

use thiserror::Error;

/// Ошибка различает exhausted budgets, security rejection и malformed XML.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum XmlReadError {
    /// Input отвергается до создания parser-а.
    #[error("XML document contains {observed} bytes with budget {maximum}")]
    DocumentBytesExceeded {
        /// Фактический размер переданного byte slice.
        observed: usize,
        /// Caller-defined maximum.
        maximum: usize,
    },
    /// Conceptual element depth превысил caller policy.
    #[error("XML depth {observed} exceeds budget {maximum}")]
    DepthExceeded {
        /// Глубина element, который не был опубликован.
        observed: usize,
        /// Caller-defined maximum.
        maximum: usize,
    },
    /// Суммарное число parser tokens превысило limit.
    #[error("XML token count {observed} exceeds budget {maximum}")]
    TokensExceeded {
        /// Число token-а, который не был опубликован.
        observed: usize,
        /// Caller-defined maximum.
        maximum: usize,
    },
    /// Один element объявил слишком много attributes.
    #[error("XML element contains {observed} attributes with budget {maximum}")]
    AttributesPerElementExceeded {
        /// Attributes текущего start/empty tag-а.
        observed: usize,
        /// Caller-defined per-element maximum.
        maximum: usize,
    },
    /// Суммарное число attributes превысило document budget.
    #[error("XML attribute count {observed} exceeds budget {maximum}")]
    AttributeCountExceeded {
        /// Accumulated document attribute count.
        observed: usize,
        /// Caller-defined maximum.
        maximum: usize,
    },
    /// Materialized attribute bytes превысили document budget.
    #[error("XML attribute bytes {observed} exceeds budget {maximum}")]
    AttributeBytesExceeded {
        /// Accumulated names, namespace URI и decoded values.
        observed: usize,
        /// Caller-defined maximum.
        maximum: usize,
    },
    /// Parser-side resolver остановил namespace allocation до выдачи event-а.
    #[error("XML element exceeds budget namespace declarations {maximum}")]
    NamespaceDeclarationsPerElementExceeded {
        /// Caller-defined per-element maximum.
        maximum: usize,
    },
    /// Суммарное число namespace declarations превысило document budget.
    #[error("XML namespace declaration count {observed} exceeds budget {maximum}")]
    NamespaceDeclarationCountExceeded {
        /// Accumulated declarations.
        observed: usize,
        /// Caller-defined maximum.
        maximum: usize,
    },
    /// Суммарные prefix/URI bytes превысили namespace budget.
    #[error("XML namespace bytes {observed} exceeds budget {maximum}")]
    NamespaceBytesExceeded {
        /// Accumulated declaration bytes.
        observed: usize,
        /// Caller-defined maximum.
        maximum: usize,
    },
    /// Decoded text content превысил document budget.
    #[error("XML text bytes {observed} exceeds budget {maximum}")]
    TextBytesExceeded {
        /// Accumulated decoded text bytes.
        observed: usize,
        /// Caller-defined maximum.
        maximum: usize,
    },
    /// Любой DOCTYPE отвергается до рассмотрения internal/external subset.
    #[error("XML DOCTYPE/DTD banned")]
    DocTypeForbidden,
    /// Custom general entity не раскрывается и не передаётся domain parser-у.
    #[error("custom XML entity forbidden")]
    CustomEntityForbidden,
    /// Malformed либо запрещённая character reference rejected typed.
    #[error("XML character reference is incorrect.")]
    InvalidCharacterReference,
    /// Boundary сознательно поддерживает только XML 1.0.
    #[error("XML declaration contains an unsupported version")]
    UnsupportedXmlVersion,
    /// Boundary принимает только UTF-8 bytes и не угадывает legacy encoding.
    #[error("XML declaration contains unsupported encoding")]
    UnsupportedEncoding,
    /// Declaration должна быть первым construct после optional UTF-8 BOM.
    #[error("The XML declaration is in an unacceptable position.")]
    MisplacedXmlDeclaration,
    /// Namespace prefix обязан иметь binding в текущем scope.
    #[error("XML namespace is incorrect")]
    InvalidNamespace,
    /// Attribute grammar, duplicate name или normalization некорректны.
    #[error("XML attribute is incorrect")]
    MalformedAttribute,
    /// Parser обнаружил malformed markup без публикации input fragment-а.
    #[error("XML document is syntactically incorrect")]
    MalformedXml,
    /// XML document обязан содержать ровно один root.
    #[error("XML document does not contain root element")]
    MissingRootElement,
    /// Второй top-level element запрещён XML document grammar.
    #[error("The XML document contains several root elements.")]
    MultipleRootElements,
    /// Non-whitespace character content вне root запрещён.
    #[error("XML text is outside the root element.")]
    TextOutsideRoot,
}
