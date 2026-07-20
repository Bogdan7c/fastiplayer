//! Caller-owned XML budgets без встроенной format-specific policy.

use std::fmt;

/// Имена обязательных budget-полей для понятной ошибки неполного builder-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlBudgetKind {
    /// Максимальный размер переданного XML document в байтах.
    DocumentBytes,
    /// Максимальная conceptual глубина element tree.
    Depth,
    /// Максимальное число parser tokens до EOF.
    Tokens,
    /// Максимальное число attributes на одном start/empty element.
    AttributesPerElement,
    /// Максимальное суммарное число attributes во всём document.
    AttributeCount,
    /// Максимальный суммарный объём materialized attribute data.
    AttributeBytes,
    /// Максимальное число namespace declarations на одном element.
    NamespaceDeclarationsPerElement,
    /// Максимальное суммарное число namespace declarations.
    NamespaceDeclarationCount,
    /// Максимальный суммарный объём namespace prefixes и URI.
    NamespaceBytes,
    /// Максимальный суммарный объём decoded text/CDATA/reference content.
    TextBytes,
}

impl fmt::Display for XmlBudgetKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Stable field names подходят и для diagnostics, и для focused assertions.
        let field_name = match self {
            Self::DocumentBytes => "document_bytes",
            Self::Depth => "depth",
            Self::Tokens => "tokens",
            Self::AttributesPerElement => "attributes_per_element",
            Self::AttributeCount => "attribute_count",
            Self::AttributeBytes => "attribute_bytes",
            Self::NamespaceDeclarationsPerElement => "namespace_declarations_per_element",
            Self::NamespaceDeclarationCount => "namespace_declaration_count",
            Self::NamespaceBytes => "namespace_bytes",
            Self::TextBytes => "text_bytes",
        };
        // Formatter получает только bounded static vocabulary.
        formatter.write_str(field_name)
    }
}

/// Ошибка сообщает, какое обязательное ограничение caller забыл определить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingXmlBudget {
    /// Поле остаётся typed, а не превращается в произвольную строку.
    field: XmlBudgetKind,
}

impl MissingXmlBudget {
    /// Возвращает точное отсутствующее budget-поле.
    pub const fn field(self) -> XmlBudgetKind {
        self.field
    }
}

impl fmt::Display for MissingXmlBudget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Сообщение не содержит входной XML и безопасно для diagnostics.
        write!(
            formatter,
            "не задан обязательный XML budget `{}`",
            self.field
        )
    }
}

impl std::error::Error for MissingXmlBudget {}

/// Полный immutable набор ограничений одного XML reader-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XmlBudgets {
    /// Byte budget проверяется до создания concrete parser-а.
    maximum_document_bytes: usize,
    /// Depth budget применяется и к обычным, и к empty elements.
    maximum_depth: usize,
    /// Token budget учитывает также comments, declarations и processing instructions.
    maximum_tokens: usize,
    /// Per-element attribute budget срабатывает до materialization domain event-а.
    maximum_attributes_per_element: usize,
    /// Document-wide count не позволяет размазать attribute bomb по многим elements.
    maximum_attribute_count: usize,
    /// Attribute bytes считают имена, resolved namespaces и decoded values.
    maximum_attribute_bytes: usize,
    /// Этот limit передаётся внутрь hardened quick-xml NamespaceResolver.
    maximum_namespace_declarations_per_element: usize,
    /// Document-wide namespace count ограничивает накопление по глубине.
    maximum_namespace_declaration_count: usize,
    /// Namespace bytes учитывают prefix и URI каждой declaration.
    maximum_namespace_bytes: usize,
    /// Text bytes считаются после безопасного predefined/numeric entity decoding.
    maximum_text_bytes: usize,
}

impl XmlBudgets {
    /// Начинает explicit builder без скрытых production defaults.
    pub const fn builder() -> XmlBudgetsBuilder {
        XmlBudgetsBuilder::new()
    }

    /// Возвращает maximum document bytes.
    pub const fn maximum_document_bytes(self) -> usize {
        self.maximum_document_bytes
    }

    /// Возвращает maximum element depth.
    pub const fn maximum_depth(self) -> usize {
        self.maximum_depth
    }

    /// Возвращает maximum parser tokens.
    pub const fn maximum_tokens(self) -> usize {
        self.maximum_tokens
    }

    /// Возвращает per-element attribute limit.
    pub const fn maximum_attributes_per_element(self) -> usize {
        self.maximum_attributes_per_element
    }

    /// Возвращает document-wide attribute count limit.
    pub const fn maximum_attribute_count(self) -> usize {
        self.maximum_attribute_count
    }

    /// Возвращает materialized attribute byte limit.
    pub const fn maximum_attribute_bytes(self) -> usize {
        self.maximum_attribute_bytes
    }

    /// Возвращает per-element namespace declaration limit.
    pub const fn maximum_namespace_declarations_per_element(self) -> usize {
        self.maximum_namespace_declarations_per_element
    }

    /// Возвращает document-wide namespace declaration count limit.
    pub const fn maximum_namespace_declaration_count(self) -> usize {
        self.maximum_namespace_declaration_count
    }

    /// Возвращает namespace byte limit.
    pub const fn maximum_namespace_bytes(self) -> usize {
        self.maximum_namespace_bytes
    }

    /// Возвращает decoded text byte limit.
    pub const fn maximum_text_bytes(self) -> usize {
        self.maximum_text_bytes
    }
}

/// Builder требует назвать каждое ограничение на месте вызова.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XmlBudgetsBuilder {
    /// `None` отличает забытое поле от осознанного нулевого budget-а.
    maximum_document_bytes: Option<usize>,
    /// Нулевой depth допустим как policy, отвергающая любой XML root.
    maximum_depth: Option<usize>,
    /// Нулевой token budget также остаётся осмысленной caller policy.
    maximum_tokens: Option<usize>,
    /// Caller может явно запретить любые attributes значением zero.
    maximum_attributes_per_element: Option<usize>,
    /// Total attribute budget задаётся независимо от per-element limit.
    maximum_attribute_count: Option<usize>,
    /// Byte accounting не подменяется одним count limit-ом.
    maximum_attribute_bytes: Option<usize>,
    /// Per-element namespace limit управляет allocation внутри parser-а.
    maximum_namespace_declarations_per_element: Option<usize>,
    /// Total namespace count управляет document-wide accumulation.
    maximum_namespace_declaration_count: Option<usize>,
    /// Namespace byte budget ограничивает длинные prefix/URI declarations.
    maximum_namespace_bytes: Option<usize>,
    /// Text byte budget применяется к Text, CDATA и entity references.
    maximum_text_bytes: Option<usize>,
}

impl XmlBudgetsBuilder {
    /// Создаёт пустой builder; `build` перечислит первое отсутствующее поле.
    pub const fn new() -> Self {
        Self {
            maximum_document_bytes: None,
            maximum_depth: None,
            maximum_tokens: None,
            maximum_attributes_per_element: None,
            maximum_attribute_count: None,
            maximum_attribute_bytes: None,
            maximum_namespace_declarations_per_element: None,
            maximum_namespace_declaration_count: None,
            maximum_namespace_bytes: None,
            maximum_text_bytes: None,
        }
    }

    /// Задаёт maximum document bytes до parser startup.
    pub const fn maximum_document_bytes(mut self, maximum: usize) -> Self {
        self.maximum_document_bytes = Some(maximum);
        self
    }

    /// Задаёт maximum conceptual element depth.
    pub const fn maximum_depth(mut self, maximum: usize) -> Self {
        self.maximum_depth = Some(maximum);
        self
    }

    /// Задаёт maximum parser token count.
    pub const fn maximum_tokens(mut self, maximum: usize) -> Self {
        self.maximum_tokens = Some(maximum);
        self
    }

    /// Задаёт maximum attributes на одном element.
    pub const fn maximum_attributes_per_element(mut self, maximum: usize) -> Self {
        self.maximum_attributes_per_element = Some(maximum);
        self
    }

    /// Задаёт maximum attributes во всём document.
    pub const fn maximum_attribute_count(mut self, maximum: usize) -> Self {
        self.maximum_attribute_count = Some(maximum);
        self
    }

    /// Задаёт maximum materialized attribute bytes.
    pub const fn maximum_attribute_bytes(mut self, maximum: usize) -> Self {
        self.maximum_attribute_bytes = Some(maximum);
        self
    }

    /// Задаёт allocation limit namespace resolver-а на одном element.
    pub const fn maximum_namespace_declarations_per_element(mut self, maximum: usize) -> Self {
        self.maximum_namespace_declarations_per_element = Some(maximum);
        self
    }

    /// Задаёт maximum namespace declarations во всём document.
    pub const fn maximum_namespace_declaration_count(mut self, maximum: usize) -> Self {
        self.maximum_namespace_declaration_count = Some(maximum);
        self
    }

    /// Задаёт maximum accumulated namespace prefix/URI bytes.
    pub const fn maximum_namespace_bytes(mut self, maximum: usize) -> Self {
        self.maximum_namespace_bytes = Some(maximum);
        self
    }

    /// Задаёт maximum decoded text bytes.
    pub const fn maximum_text_bytes(mut self, maximum: usize) -> Self {
        self.maximum_text_bytes = Some(maximum);
        self
    }

    /// Завершает builder только после explicit определения каждого budget-а.
    pub fn build(self) -> Result<XmlBudgets, MissingXmlBudget> {
        // Маленький helper сохраняет однообразную typed ошибку для всех полей.
        fn required(value: Option<usize>, field: XmlBudgetKind) -> Result<usize, MissingXmlBudget> {
            value.ok_or(MissingXmlBudget { field })
        }

        // Struct literal оставляет соответствие каждого caller intent своему полю видимым.
        Ok(XmlBudgets {
            maximum_document_bytes: required(
                self.maximum_document_bytes,
                XmlBudgetKind::DocumentBytes,
            )?,
            maximum_depth: required(self.maximum_depth, XmlBudgetKind::Depth)?,
            maximum_tokens: required(self.maximum_tokens, XmlBudgetKind::Tokens)?,
            maximum_attributes_per_element: required(
                self.maximum_attributes_per_element,
                XmlBudgetKind::AttributesPerElement,
            )?,
            maximum_attribute_count: required(
                self.maximum_attribute_count,
                XmlBudgetKind::AttributeCount,
            )?,
            maximum_attribute_bytes: required(
                self.maximum_attribute_bytes,
                XmlBudgetKind::AttributeBytes,
            )?,
            maximum_namespace_declarations_per_element: required(
                self.maximum_namespace_declarations_per_element,
                XmlBudgetKind::NamespaceDeclarationsPerElement,
            )?,
            maximum_namespace_declaration_count: required(
                self.maximum_namespace_declaration_count,
                XmlBudgetKind::NamespaceDeclarationCount,
            )?,
            maximum_namespace_bytes: required(
                self.maximum_namespace_bytes,
                XmlBudgetKind::NamespaceBytes,
            )?,
            maximum_text_bytes: required(self.maximum_text_bytes, XmlBudgetKind::TextBytes)?,
        })
    }
}

impl Default for XmlBudgetsBuilder {
    fn default() -> Self {
        // Default builder намеренно не подставляет скрытые numerical policy values.
        Self::new()
    }
}
