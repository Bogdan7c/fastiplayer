//! Budgeted adapter над quick-xml 0.41 `NsReader`.

use std::collections::HashSet;

use quick_xml::events::{BytesDecl, BytesRef, BytesStart, Event};
use quick_xml::name::{NamespaceError, ResolveResult};
use quick_xml::{Decoder, Error as QuickXmlError, NsReader, XmlVersion};

use crate::budget::XmlBudgets;
use crate::error::XmlReadError;
use crate::event::{XmlAttribute, XmlElement, XmlEvent, XmlExpandedName, XmlText};

/// XML namespace declaration marker.
const XMLNS_ATTRIBUTE_NAME: &[u8] = b"xmlns";
/// Prefix marker отделяет namespace declaration от обычного attribute.
const XMLNS_ATTRIBUTE_PREFIX: &[u8] = b"xmlns:";
/// Encoding feature намеренно выключен: boundary принимает только exact UTF-8.
const UTF8_ENCODING_NAME: &[u8] = b"utf-8";

/// Reader lifecycle не позволяет продолжить parsing после security/error outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReaderTerminalState {
    /// Reader может получить следующий raw token.
    Active,
    /// EOF уже провалидирован; повторные вызовы возвращают `Ok(None)`.
    Complete,
    /// Первый failure сохраняется и детерминированно повторяется.
    Failed(XmlReadError),
}

/// Положение относительно единственного XML document root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootState {
    /// Root element ещё не встречен.
    Before,
    /// Reader находится внутри root element.
    Inside,
    /// Matching root end уже прочитан.
    After,
}

/// Mutable accounting отделён от immutable caller policy.
#[derive(Debug, Default)]
struct XmlAccounting {
    /// Текущая conceptual element depth.
    depth: usize,
    /// Суммарное число raw tokens кроме EOF.
    token_count: usize,
    /// Суммарное число attributes, включая namespace declarations.
    attribute_count: usize,
    /// Суммарный materialized attribute byte charge.
    attribute_bytes: usize,
    /// Суммарное число namespace declarations.
    namespace_declaration_count: usize,
    /// Суммарные namespace prefix/URI bytes.
    namespace_bytes: usize,
    /// Суммарные decoded text bytes.
    text_bytes: usize,
}

/// Единственный public reader недоверенного XML.
pub struct BoundedXmlReader<'input> {
    /// Slice-backed reader гарантирует отсутствие hidden filesystem/network I/O.
    reader: NsReader<&'input [u8]>,
    /// Caller policy immutable на протяжении document.
    budgets: XmlBudgets,
    /// Mutable counters принадлежат boundary, а не domain parser-у.
    accounting: XmlAccounting,
    /// Root state усиливает tokenizer до well-formed single-root document boundary.
    root_state: RootState,
    /// XML version управляет line-ending и attribute normalization.
    xml_version: XmlVersion,
    /// Declaration может появиться только один раз до root content.
    declaration_seen: bool,
    /// XML declaration разрешена только до первого non-declaration token-а.
    declaration_allowed: bool,
    /// Terminal state делает failure path fail-closed.
    terminal_state: ReaderTerminalState,
}

impl<'input> BoundedXmlReader<'input> {
    /// Создаёт reader только из caller-provided bytes и explicit budgets.
    pub fn new(input: &'input [u8], budgets: XmlBudgets) -> Result<Self, XmlReadError> {
        // Byte budget срабатывает до любых parser allocations.
        if input.len() > budgets.maximum_document_bytes() {
            return Err(XmlReadError::DocumentBytesExceeded {
                observed: input.len(),
                maximum: budgets.maximum_document_bytes(),
            });
        }

        // NsReader выполняет namespace resolution, но не знает domain schema.
        let mut reader = NsReader::from_reader(input);
        // Hardened 0.41 knob ограничивает allocation до выдачи Start/Empty event-а.
        reader
            .resolver_mut()
            .set_max_declarations_per_element(budgets.maximum_namespace_declarations_per_element());

        // Все mutable invariants и counters создаются в одном owner-е.
        Ok(Self {
            reader,
            budgets,
            accounting: XmlAccounting::default(),
            root_state: RootState::Before,
            xml_version: XmlVersion::Implicit1_0,
            declaration_seen: false,
            declaration_allowed: true,
            terminal_state: ReaderTerminalState::Active,
        })
    }

    /// Возвращает следующий domain-neutral event либо validated EOF.
    pub fn next_event(&mut self) -> Result<Option<XmlEvent>, XmlReadError> {
        // Complete reader ведёт себя как обычный fused stream.
        if self.terminal_state == ReaderTerminalState::Complete {
            return Ok(None);
        }
        // Failed reader повторяет exact typed outcome и не читает новые bytes.
        if let ReaderTerminalState::Failed(error) = &self.terminal_state {
            return Err(error.clone());
        }

        // Inner loop пропускает безопасные comments/declaration/PI после accounting.
        loop {
            // Один вызов concrete parser-а остаётся внутри project-owned boundary.
            let next_result = self.next_event_inner();
            // Любая ошибка terminal-ит reader до возврата caller-у.
            match next_result {
                Ok(Some(event)) => return Ok(Some(event)),
                Ok(None) if self.terminal_state == ReaderTerminalState::Complete => {
                    return Ok(None);
                }
                Ok(None) => continue,
                Err(error) => {
                    self.terminal_state = ReaderTerminalState::Failed(error.clone());
                    return Err(error);
                }
            }
        }
    }

    /// Обрабатывает ровно один raw parser token.
    fn next_event_inner(&mut self) -> Result<Option<XmlEvent>, XmlReadError> {
        // Slice reader возвращает events, заимствующие immutable input, а не hidden I/O buffer.
        let raw_event = self.reader.read_event().map_err(map_quick_xml_error)?;
        // EOF не считается token-ом, но завершает document grammar validation.
        if matches!(raw_event, Event::Eof) {
            return self.finish_document();
        }
        // Каждый прочитанный raw construct участвует в token budget.
        self.account_token()?;
        // Любой другой construct закрывает grammar slot XML declaration.
        if !matches!(raw_event, Event::Decl(_)) {
            self.declaration_allowed = false;
        }

        // Event dispatch сохраняет format-neutral XML semantics.
        match raw_event {
            Event::Start(start) => self.process_start(start).map(Some),
            Event::Empty(empty) => self.process_empty(empty).map(Some),
            Event::End(end) => {
                let name = expanded_name(
                    self.reader.resolver().resolve_element(end.name()).0,
                    end.local_name().as_ref(),
                    self.reader.decoder(),
                )?;
                self.process_end(name).map(Some)
            }
            Event::Text(text) => {
                let content = text
                    .xml_content(self.xml_version)
                    .map_err(|_| XmlReadError::MalformedXml)?
                    .into_owned();
                self.process_text(content)
            }
            Event::CData(cdata) => {
                let content = cdata
                    .xml_content(self.xml_version)
                    .map_err(|_| XmlReadError::MalformedXml)?
                    .into_owned();
                self.process_text(content)
            }
            Event::GeneralRef(reference) => {
                let content = resolve_reference(&reference)?;
                self.process_text(content)
            }
            Event::Decl(declaration) => {
                self.process_declaration(&declaration)?;
                Ok(None)
            }
            Event::DocType(_) => Err(XmlReadError::DocTypeForbidden),
            Event::Comment(_) | Event::PI(_) => Ok(None),
            Event::Eof => unreachable!("EOF handled before token accounting"),
        }
    }

    /// Start element проверяется до публикации caller-у.
    fn process_start(&mut self, start: BytesStart<'input>) -> Result<XmlEvent, XmlReadError> {
        // Второй root detected до изменения depth.
        if self.root_state == RootState::After {
            return Err(XmlReadError::MultipleRootElements);
        }
        // Conceptual depth увеличивается checked arithmetic-ом.
        let observed_depth =
            self.accounting
                .depth
                .checked_add(1)
                .ok_or(XmlReadError::DepthExceeded {
                    observed: usize::MAX,
                    maximum: self.budgets.maximum_depth(),
                })?;
        // Element, который превышает limit, не попадает в domain stream.
        if observed_depth > self.budgets.maximum_depth() {
            return Err(XmlReadError::DepthExceeded {
                observed: observed_depth,
                maximum: self.budgets.maximum_depth(),
            });
        }
        // Attributes и namespaces materialize только после parser-side namespace cap.
        let element = self.materialize_element(&start)?;
        // Первый start открывает единственный root.
        self.root_state = RootState::Inside;
        // Depth commit происходит вместе с успешной публикацией event-а.
        self.accounting.depth = observed_depth;
        Ok(XmlEvent::StartElement(element))
    }

    /// Empty element учитывает conceptual child depth, но не оставляет scope открытым.
    fn process_empty(&mut self, empty: BytesStart<'input>) -> Result<XmlEvent, XmlReadError> {
        // После закрытого root новый empty element также является вторым root.
        if self.root_state == RootState::After {
            return Err(XmlReadError::MultipleRootElements);
        }
        // Empty child существует на один conceptual level глубже текущего scope.
        let observed_depth =
            self.accounting
                .depth
                .checked_add(1)
                .ok_or(XmlReadError::DepthExceeded {
                    observed: usize::MAX,
                    maximum: self.budgets.maximum_depth(),
                })?;
        // Проверка выполняется до materialized event.
        if observed_depth > self.budgets.maximum_depth() {
            return Err(XmlReadError::DepthExceeded {
                observed: observed_depth,
                maximum: self.budgets.maximum_depth(),
            });
        }
        // Element получает те же attribute/namespace guarantees, что обычный Start.
        let element = self.materialize_element(&empty)?;
        // Empty element на top level одновременно является root и закрывает его.
        if self.root_state == RootState::Before {
            self.root_state = RootState::After;
        }
        Ok(XmlEvent::EmptyElement(element))
    }

    /// End element завершает текущий scope с checked accounting.
    fn process_end(&mut self, name: XmlExpandedName) -> Result<XmlEvent, XmlReadError> {
        // Concrete parser проверяет matching tag names; zero защищает boundary invariant.
        let remaining_depth = self
            .accounting
            .depth
            .checked_sub(1)
            .ok_or(XmlReadError::MalformedXml)?;
        // Root закрывается только когда глубина возвращается к zero.
        if remaining_depth == 0 {
            self.root_state = RootState::After;
        }
        // Depth commit происходит вместе с успешным End event.
        self.accounting.depth = remaining_depth;
        Ok(XmlEvent::EndElement(name))
    }

    /// Text accounting применяется одинаково к Text, CDATA и legal references.
    fn process_text(&mut self, content: String) -> Result<Option<XmlEvent>, XmlReadError> {
        // Decoded bytes, а не raw escape spelling, определяют downstream allocation.
        let observed_text_bytes = checked_accumulate(
            self.accounting.text_bytes,
            content.len(),
            self.budgets.maximum_text_bytes(),
            |observed, maximum| XmlReadError::TextBytesExceeded { observed, maximum },
        )?;
        // Accounting commit предшествует любому skip whitespace.
        self.accounting.text_bytes = observed_text_bytes;

        // XML разрешает только whitespace до/после document element.
        if self.root_state != RootState::Inside {
            if content.chars().all(is_xml_markup_whitespace) {
                return Ok(None);
            }
            return Err(XmlReadError::TextOutsideRoot);
        }
        // Empty chunks не несут domain information.
        if content.is_empty() {
            return Ok(None);
        }
        // Domain parser получает bounded decoded text.
        Ok(Some(XmlEvent::Text(XmlText::new(content))))
    }

    /// Declaration задаёт normalization version, но не domain schema.
    fn process_declaration(&mut self, declaration: &BytesDecl<'_>) -> Result<(), XmlReadError> {
        // Declaration разрешена ровно один раз до любого root content.
        if self.declaration_seen
            || !self.declaration_allowed
            || self.root_state != RootState::Before
        {
            return Err(XmlReadError::MisplacedXmlDeclaration);
        }
        // BytesDecl helper-ы не валидируют duplicate/order всех pseudo-attributes,
        // поэтому boundary повторно проходит bounded declaration с checks enabled.
        let declaration_content = self
            .reader
            .decoder()
            .decode(declaration)
            .map_err(|_| XmlReadError::MalformedXml)?
            .into_owned();
        // Reader гарантирует `xml` target; name length нужен только attribute iterator-у.
        let declaration_start = BytesStart::from_content(declaration_content, 3);
        // quick-xml 0.41 duplicate checking линейно и безопасно для untrusted input.
        let mut attributes = declaration_start.attributes();

        // Version является обязательным первым pseudo-attribute declaration.
        let version = attributes
            .next()
            .ok_or(XmlReadError::MalformedXml)?
            .map_err(|_| XmlReadError::MalformedAttribute)?;
        if version.key.as_ref() != b"version" {
            return Err(XmlReadError::MalformedXml);
        }
        // XML 1.1 не заявляется частично: расширение contract требует отдельного решения.
        if version.value.as_ref() != b"1.0" {
            return Err(XmlReadError::UnsupportedXmlVersion);
        }

        // Encoding, если присутствует, обязана следовать сразу после version.
        let mut next_attribute = attributes
            .next()
            .transpose()
            .map_err(|_| XmlReadError::MalformedAttribute)?;
        if let Some(encoding) = next_attribute.as_ref()
            && encoding.key.as_ref() == b"encoding"
        {
            if !encoding
                .value
                .as_ref()
                .eq_ignore_ascii_case(UTF8_ENCODING_NAME)
            {
                return Err(XmlReadError::UnsupportedEncoding);
            }
            next_attribute = attributes
                .next()
                .transpose()
                .map_err(|_| XmlReadError::MalformedAttribute)?;
        }

        // Standalone является единственным допустимым последним pseudo-attribute.
        if let Some(standalone) = next_attribute
            && (standalone.key.as_ref() != b"standalone"
                || !matches!(standalone.value.as_ref(), b"yes" | b"no"))
        {
            return Err(XmlReadError::MalformedAttribute);
        }
        // Любой четвёртый/duplicate/unknown pseudo-attribute нарушает declaration grammar.
        if attributes
            .next()
            .transpose()
            .map_err(|_| XmlReadError::MalformedAttribute)?
            .is_some()
        {
            return Err(XmlReadError::MalformedAttribute);
        }

        // Успешная declaration фиксирует XML 1.0 normalization.
        self.xml_version = XmlVersion::Explicit1_0;
        // Commit не происходит при malformed/unsupported declaration.
        self.declaration_seen = true;
        // После единственной declaration второй declaration уже недопустим.
        self.declaration_allowed = false;
        Ok(())
    }

    /// Materialize element отделяет namespace mechanics от domain schema.
    fn materialize_element(
        &mut self,
        start: &BytesStart<'input>,
    ) -> Result<XmlElement, XmlReadError> {
        // Decoder соответствует exact slice-backed reader.
        let decoder = self.reader.decoder();
        // Element default namespace уже установлен NsReader-ом до выдачи event-а.
        let (element_namespace, local_name) = self.reader.resolver().resolve_element(start.name());
        // Expanded name не сохраняет syntactic prefix.
        let name = expanded_name(element_namespace, local_name.as_ref(), decoder)?;
        // Vec capacity не превышает parser-observed count и caller per-element budget.
        let mut attributes = Vec::new();
        // Namespace declarations считаются отдельно, хотя parser также видит их attributes.
        let mut element_attribute_count = 0usize;

        // Checks остаются включены: quick-xml 0.41 делает duplicate detection linear.
        for attribute_result in start.attributes() {
            // Malformed/duplicate attribute не игнорируется.
            let attribute = attribute_result.map_err(|_| XmlReadError::MalformedAttribute)?;
            // Per-element count checked до materialization очередного value.
            element_attribute_count = element_attribute_count.checked_add(1).ok_or(
                XmlReadError::AttributesPerElementExceeded {
                    observed: usize::MAX,
                    maximum: self.budgets.maximum_attributes_per_element(),
                },
            )?;
            if element_attribute_count > self.budgets.maximum_attributes_per_element() {
                return Err(XmlReadError::AttributesPerElementExceeded {
                    observed: element_attribute_count,
                    maximum: self.budgets.maximum_attributes_per_element(),
                });
            }
            // Document count не позволяет распределить bomb по множеству elements.
            let observed_attribute_count = checked_accumulate(
                self.accounting.attribute_count,
                1,
                self.budgets.maximum_attribute_count(),
                |observed, maximum| XmlReadError::AttributeCountExceeded { observed, maximum },
            )?;
            // Attribute normalization раскрывает только numeric/predefined XML references.
            let value = attribute
                .decoded_and_normalized_value(self.xml_version, decoder)
                .map_err(map_quick_xml_error)?
                .into_owned();
            // Namespace declaration не публикуется как domain attribute.
            if let Some(prefix) = namespace_declaration_prefix(attribute.key.as_ref()) {
                self.account_namespace_declaration(prefix, &value)?;
                self.accounting.attribute_count = observed_attribute_count;
                continue;
            }

            // Default namespace не применяется к unprefixed attributes.
            let (namespace, local_name) = self.reader.resolver().resolve_attribute(attribute.key);
            // Unknown prefix становится typed namespace failure.
            let attribute_name = expanded_name(namespace, local_name.as_ref(), decoder)?;
            // Byte charge включает повторно materialized namespace URI.
            let materialized_bytes = expanded_name_bytes(&attribute_name)
                .checked_add(value.len())
                .ok_or(XmlReadError::AttributeBytesExceeded {
                    observed: usize::MAX,
                    maximum: self.budgets.maximum_attribute_bytes(),
                })?;
            // Total bytes проверяются до Vec push.
            let observed_attribute_bytes = checked_accumulate(
                self.accounting.attribute_bytes,
                materialized_bytes,
                self.budgets.maximum_attribute_bytes(),
                |observed, maximum| XmlReadError::AttributeBytesExceeded { observed, maximum },
            )?;
            // Counters commit вместе с успешным owned attribute.
            self.accounting.attribute_count = observed_attribute_count;
            self.accounting.attribute_bytes = observed_attribute_bytes;
            attributes.push(XmlAttribute::new(attribute_name, value));
        }

        // XML Namespaces запрещает duplicates по expanded name, даже когда raw prefixes разные.
        let mut unique_attribute_names = HashSet::with_capacity(attributes.len());
        // HashSet allocation остаётся bounded caller-defined per-element attribute limit-ом.
        for attribute in &attributes {
            // Randomized std hasher сохраняет expected linear behavior на untrusted именах.
            if !unique_attribute_names.insert(attribute.name()) {
                return Err(XmlReadError::MalformedAttribute);
            }
        }

        // Element публикуется только после полного start-tag accounting.
        Ok(XmlElement::new(name, attributes))
    }

    /// Namespace declaration accounting идёт поверх parser-side per-element cap.
    fn account_namespace_declaration(
        &mut self,
        prefix: &[u8],
        namespace_uri: &str,
    ) -> Result<(), XmlReadError> {
        // Total count checked до commit.
        let observed_count = checked_accumulate(
            self.accounting.namespace_declaration_count,
            1,
            self.budgets.maximum_namespace_declaration_count(),
            |observed, maximum| XmlReadError::NamespaceDeclarationCountExceeded {
                observed,
                maximum,
            },
        )?;
        // Prefix и decoded URI отражают parser-owned resolver payload.
        let declaration_bytes = prefix.len().checked_add(namespace_uri.len()).ok_or(
            XmlReadError::NamespaceBytesExceeded {
                observed: usize::MAX,
                maximum: self.budgets.maximum_namespace_bytes(),
            },
        )?;
        // Total namespace bytes checked до commit.
        let observed_bytes = checked_accumulate(
            self.accounting.namespace_bytes,
            declaration_bytes,
            self.budgets.maximum_namespace_bytes(),
            |observed, maximum| XmlReadError::NamespaceBytesExceeded { observed, maximum },
        )?;
        // Оба counters commit атомарно после успешных checks.
        self.accounting.namespace_declaration_count = observed_count;
        self.accounting.namespace_bytes = observed_bytes;
        Ok(())
    }

    /// Token accounting единообразно применяется ко всем non-EOF constructs.
    fn account_token(&mut self) -> Result<(), XmlReadError> {
        // Checked addition не допускает wrap в release build.
        let observed =
            self.accounting
                .token_count
                .checked_add(1)
                .ok_or(XmlReadError::TokensExceeded {
                    observed: usize::MAX,
                    maximum: self.budgets.maximum_tokens(),
                })?;
        // Token, превысивший budget, не обрабатывается дальше.
        if observed > self.budgets.maximum_tokens() {
            return Err(XmlReadError::TokensExceeded {
                observed,
                maximum: self.budgets.maximum_tokens(),
            });
        }
        // Commit сохраняет exact число успешно допущенных raw tokens.
        self.accounting.token_count = observed;
        Ok(())
    }

    /// EOF валидирует root/depth invariants и terminal-ит reader.
    fn finish_document(&mut self) -> Result<Option<XmlEvent>, XmlReadError> {
        // Tokenizer мог закончить bytes без document element.
        if self.root_state == RootState::Before {
            return Err(XmlReadError::MissingRootElement);
        }
        // Незакрытый scope не считается successful EOF.
        if self.accounting.depth != 0 || self.root_state != RootState::After {
            return Err(XmlReadError::MalformedXml);
        }
        // Fused completion не читает input повторно.
        self.terminal_state = ReaderTerminalState::Complete;
        Ok(None)
    }
}

/// XML document grammar вне root допускает только четыре `S` characters.
fn is_xml_markup_whitespace(character: char) -> bool {
    // Unicode NBSP и прочие `char::is_whitespace` сюда намеренно не входят.
    matches!(character, ' ' | '\t' | '\n' | '\r')
}

/// Преобразует parser resolution в owned project vocabulary.
fn expanded_name(
    resolution: ResolveResult<'_>,
    local_name: &[u8],
    decoder: Decoder,
) -> Result<XmlExpandedName, XmlReadError> {
    // Unknown prefix никогда не превращается в unbound name.
    let namespace_uri = match resolution {
        ResolveResult::Unbound => None,
        ResolveResult::Bound(namespace) => Some(
            decoder
                .decode(namespace.as_ref())
                .map_err(|_| XmlReadError::MalformedXml)?
                .into_owned(),
        ),
        ResolveResult::Unknown(_) => return Err(XmlReadError::InvalidNamespace),
    };
    // Local name decoder не угадывает alternate encodings.
    let local_name = decoder
        .decode(local_name)
        .map_err(|_| XmlReadError::MalformedXml)?
        .into_owned();
    Ok(XmlExpandedName::new(namespace_uri, local_name))
}

/// Возвращает namespace prefix для `xmlns`/`xmlns:*`, иначе `None`.
fn namespace_declaration_prefix(attribute_name: &[u8]) -> Option<&[u8]> {
    // Default namespace declaration имеет пустой syntactic prefix.
    if attribute_name == XMLNS_ATTRIBUTE_NAME {
        return Some(&[]);
    }
    // Prefixed declaration хранит только suffix после `xmlns:`.
    attribute_name.strip_prefix(XMLNS_ATTRIBUTE_PREFIX)
}

/// Считает bytes owned expanded name без синтаксического prefix-а.
fn expanded_name_bytes(name: &XmlExpandedName) -> usize {
    // Saturating value затем checked_add-ится caller-ом и сравнивается с budget.
    name.local_name()
        .len()
        .saturating_add(name.namespace_uri().map_or(0, str::len))
}

/// Разрешает только numeric и пять predefined XML references.
fn resolve_reference(reference: &BytesRef<'_>) -> Result<String, XmlReadError> {
    // Numeric reference валидируется parser helper-ом.
    if let Some(character) = reference
        .resolve_char_ref()
        .map_err(|_| XmlReadError::InvalidCharacterReference)?
    {
        // XML legal-character subset проверяется отдельно от Unicode scalar validity.
        if !is_legal_xml_character(character) {
            return Err(XmlReadError::InvalidCharacterReference);
        }
        return Ok(character.to_string());
    }
    // General reference name декодируется как bounded token payload.
    let reference_name = reference.decode().map_err(|_| XmlReadError::MalformedXml)?;
    // Только predefined XML entities являются legal без DTD.
    let predefined = match reference_name.as_ref() {
        "lt" => "<",
        "gt" => ">",
        "amp" => "&",
        "apos" => "'",
        "quot" => "\"",
        _ => return Err(XmlReadError::CustomEntityForbidden),
    };
    Ok(predefined.to_owned())
}

/// Проверяет XML 1.0 legal character ranges, достаточные и для supported documents.
fn is_legal_xml_character(character: char) -> bool {
    // XML 1.0 разрешает tab/LF/CR и остальные scalar ranges без surrogate code points.
    matches!(
        character,
        '\u{9}' | '\u{A}' | '\u{D}'
            | '\u{20}'..='\u{D7FF}'
            | '\u{E000}'..='\u{FFFD}'
            | '\u{10000}'..='\u{10FFFF}'
    )
}

/// Checked accumulated counter с caller-specific typed error constructor.
fn checked_accumulate(
    current: usize,
    addition: usize,
    maximum: usize,
    error: impl Fn(usize, usize) -> XmlReadError,
) -> Result<usize, XmlReadError> {
    // Overflow отображается как максимально возможное observed значение.
    let observed = current
        .checked_add(addition)
        .ok_or_else(|| error(usize::MAX, maximum))?;
    // Limit failure не коммитит counter.
    if observed > maximum {
        return Err(error(observed, maximum));
    }
    Ok(observed)
}

/// Сжимает concrete parser failures в stable bounded project vocabulary.
fn map_quick_xml_error(error: QuickXmlError) -> XmlReadError {
    // Parser-specific strings не попадают наружу и не могут утечь в diagnostics.
    match error {
        QuickXmlError::Namespace(NamespaceError::TooManyDeclarations(maximum)) => {
            XmlReadError::NamespaceDeclarationsPerElementExceeded { maximum }
        }
        QuickXmlError::Namespace(_) => XmlReadError::InvalidNamespace,
        QuickXmlError::InvalidAttr(_) => XmlReadError::MalformedAttribute,
        QuickXmlError::Escape(quick_xml::escape::EscapeError::UnrecognizedEntity(_, _)) => {
            XmlReadError::CustomEntityForbidden
        }
        QuickXmlError::Escape(quick_xml::escape::EscapeError::InvalidCharRef(_)) => {
            XmlReadError::InvalidCharacterReference
        }
        QuickXmlError::Encoding(_) => XmlReadError::MalformedXml,
        QuickXmlError::Io(_)
        | QuickXmlError::Syntax(_)
        | QuickXmlError::IllFormed(_)
        | QuickXmlError::Escape(_) => XmlReadError::MalformedXml,
    }
}
