//! Focused malicious fixtures и caller-defined budget contracts.

use bounded_xml_reader::{
    BoundedXmlReader, XmlBudgetKind, XmlBudgets, XmlElement, XmlEvent, XmlReadError,
};

/// Общий generous budget остаётся test-only и не создаёт production defaults.
fn generous_budgets() -> XmlBudgets {
    XmlBudgets::builder()
        .maximum_document_bytes(64 * 1024)
        .maximum_depth(32)
        .maximum_tokens(512)
        .maximum_attributes_per_element(32)
        .maximum_attribute_count(128)
        .maximum_attribute_bytes(16 * 1024)
        .maximum_namespace_declarations_per_element(16)
        .maximum_namespace_declaration_count(64)
        .maximum_namespace_bytes(8 * 1024)
        .maximum_text_bytes(32 * 1024)
        .build()
        .expect("test задаёт каждое обязательное поле")
}

/// Читает весь event stream и сохраняет exact terminal error semantics.
fn collect_events(xml_bytes: &[u8], budgets: XmlBudgets) -> Result<Vec<XmlEvent>, XmlReadError> {
    let mut reader = BoundedXmlReader::new(xml_bytes, budgets)?;
    let mut events = Vec::new();
    while let Some(event) = reader.next_event()? {
        events.push(event);
    }
    Ok(events)
}

/// Извлекает Start/Empty element для compact focused assertions.
fn event_element(event: &XmlEvent) -> &XmlElement {
    match event {
        XmlEvent::StartElement(element) | XmlEvent::EmptyElement(element) => element,
        XmlEvent::EndElement(_) | XmlEvent::Text(_) => {
            panic!("ожидался element event")
        }
    }
}

#[test]
fn namespace_resolved_events_keep_domain_schema_outside_reader() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
        <playlist xmlns="urn:test" xmlns:x="urn:ext" xml:base="https://example.invalid/">
            <x:track title="A &amp; B">a&lt;b&#x21;&apos;&quot;&gt;</x:track>
        </playlist>"#;
    let events = collect_events(xml, generous_budgets()).expect("валидный XML читается");

    let root = event_element(&events[0]);
    assert_eq!(root.name().namespace_uri(), Some("urn:test"));
    assert_eq!(root.name().local_name(), "playlist");
    assert_eq!(root.attributes().len(), 1);
    assert_eq!(
        root.attributes()[0].name().namespace_uri(),
        Some("http://www.w3.org/XML/1998/namespace")
    );
    assert_eq!(root.attributes()[0].name().local_name(), "base");

    let track = event_element(&events[2]);
    assert_eq!(track.name().namespace_uri(), Some("urn:ext"));
    assert_eq!(track.name().local_name(), "track");
    assert_eq!(track.attributes()[0].value(), "A & B");

    let text = events
        .iter()
        .filter_map(|event| match event {
            XmlEvent::Text(text) => Some(text.content()),
            _ => None,
        })
        .filter(|content| !content.trim().is_empty())
        .collect::<String>();
    assert_eq!(text, "a<b!'\">");
}

#[test]
fn doctype_and_external_entity_fixture_is_rejected_without_resolution() {
    let malicious_xml = include_bytes!("fixtures/doctype_external.xml");
    let error = collect_events(malicious_xml, generous_budgets())
        .expect_err("DOCTYPE обязан быть rejected до external resolution");
    assert_eq!(error, XmlReadError::DocTypeForbidden);
}

#[test]
fn internal_custom_entity_fixture_is_rejected_at_doctype_boundary() {
    let malicious_xml = include_bytes!("fixtures/doctype_internal_entity.xml");
    let error = collect_events(malicious_xml, generous_budgets())
        .expect_err("internal entity declaration не разрешена");
    assert_eq!(error, XmlReadError::DocTypeForbidden);
}

#[test]
fn undeclared_custom_entity_fixture_is_rejected_but_predefined_entities_are_legal() {
    let malicious_xml = include_bytes!("fixtures/custom_entity_reference.xml");
    let error = collect_events(malicious_xml, generous_budgets())
        .expect_err("custom general reference не раскрывается");
    assert_eq!(error, XmlReadError::CustomEntityForbidden);

    let legal_xml = br#"<root>&lt;&gt;&amp;&apos;&quot;&#65;&#x42;</root>"#;
    let events = collect_events(legal_xml, generous_budgets())
        .expect("predefined и numeric references разрешены");
    let text = events
        .iter()
        .filter_map(|event| match event {
            XmlEvent::Text(text) => Some(text.content()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(text, "<>&'\"AB");
}

#[test]
fn depth_fixture_is_stopped_at_caller_limit() {
    let malicious_xml = include_bytes!("fixtures/deep_nesting.xml");
    let budgets = XmlBudgets::builder()
        .maximum_document_bytes(4096)
        .maximum_depth(4)
        .maximum_tokens(64)
        .maximum_attributes_per_element(4)
        .maximum_attribute_count(16)
        .maximum_attribute_bytes(1024)
        .maximum_namespace_declarations_per_element(4)
        .maximum_namespace_declaration_count(8)
        .maximum_namespace_bytes(1024)
        .maximum_text_bytes(1024)
        .build()
        .expect("test задаёт complete budgets");
    let error = collect_events(malicious_xml, budgets)
        .expect_err("пятый depth level обязан превысить caller limit");
    assert_eq!(
        error,
        XmlReadError::DepthExceeded {
            observed: 5,
            maximum: 4,
        }
    );
}

#[test]
fn attribute_bomb_fixture_is_stopped_before_domain_event_materialization() {
    let malicious_xml = include_bytes!("fixtures/attribute_bomb.xml");
    let budgets = XmlBudgets::builder()
        .maximum_document_bytes(4096)
        .maximum_depth(4)
        .maximum_tokens(32)
        .maximum_attributes_per_element(8)
        .maximum_attribute_count(64)
        .maximum_attribute_bytes(4096)
        .maximum_namespace_declarations_per_element(4)
        .maximum_namespace_declaration_count(8)
        .maximum_namespace_bytes(1024)
        .maximum_text_bytes(1024)
        .build()
        .expect("test задаёт complete budgets");
    let error = collect_events(malicious_xml, budgets)
        .expect_err("девятый attribute обязан остановить start tag");
    assert_eq!(
        error,
        XmlReadError::AttributesPerElementExceeded {
            observed: 9,
            maximum: 8,
        }
    );
}

#[test]
fn namespace_bomb_fixture_hits_parser_side_limit_before_event() {
    let malicious_xml = include_bytes!("fixtures/namespace_bomb.xml");
    let budgets = XmlBudgets::builder()
        .maximum_document_bytes(4096)
        .maximum_depth(4)
        .maximum_tokens(32)
        .maximum_attributes_per_element(32)
        .maximum_attribute_count(64)
        .maximum_attribute_bytes(4096)
        .maximum_namespace_declarations_per_element(4)
        .maximum_namespace_declaration_count(32)
        .maximum_namespace_bytes(4096)
        .maximum_text_bytes(1024)
        .build()
        .expect("test задаёт complete budgets");
    let error = collect_events(malicious_xml, budgets)
        .expect_err("NsReader обязан остановить allocation до выдачи event-а");
    assert_eq!(
        error,
        XmlReadError::NamespaceDeclarationsPerElementExceeded { maximum: 4 }
    );
}

#[test]
fn document_wide_namespace_count_and_bytes_are_independently_bounded() {
    let xml = br#"<root xmlns:a="urn:a"><child xmlns:b="urn:b"/></root>"#;
    let count_budgets = XmlBudgets::builder()
        .maximum_document_bytes(4096)
        .maximum_depth(4)
        .maximum_tokens(32)
        .maximum_attributes_per_element(8)
        .maximum_attribute_count(16)
        .maximum_attribute_bytes(4096)
        .maximum_namespace_declarations_per_element(4)
        .maximum_namespace_declaration_count(1)
        .maximum_namespace_bytes(4096)
        .maximum_text_bytes(1024)
        .build()
        .expect("test задаёт complete budgets");
    assert_eq!(
        collect_events(xml, count_budgets).expect_err("вторая declaration превышает count"),
        XmlReadError::NamespaceDeclarationCountExceeded {
            observed: 2,
            maximum: 1,
        }
    );

    let byte_budgets = XmlBudgets::builder()
        .maximum_document_bytes(4096)
        .maximum_depth(4)
        .maximum_tokens(32)
        .maximum_attributes_per_element(8)
        .maximum_attribute_count(16)
        .maximum_attribute_bytes(4096)
        .maximum_namespace_declarations_per_element(4)
        .maximum_namespace_declaration_count(8)
        .maximum_namespace_bytes(5)
        .maximum_text_bytes(1024)
        .build()
        .expect("test задаёт complete budgets");
    assert!(matches!(
        collect_events(xml, byte_budgets),
        Err(XmlReadError::NamespaceBytesExceeded { .. })
    ));
}

#[test]
fn caller_defined_document_token_attribute_and_text_budgets_are_independent() {
    let xml = br#"<root a="123"><child>text</child></root>"#;

    let small_document = XmlBudgets::builder()
        .maximum_document_bytes(xml.len() - 1)
        .maximum_depth(8)
        .maximum_tokens(32)
        .maximum_attributes_per_element(8)
        .maximum_attribute_count(8)
        .maximum_attribute_bytes(64)
        .maximum_namespace_declarations_per_element(4)
        .maximum_namespace_declaration_count(4)
        .maximum_namespace_bytes(64)
        .maximum_text_bytes(64)
        .build()
        .expect("test задаёт complete budgets");
    assert!(matches!(
        BoundedXmlReader::new(xml, small_document),
        Err(XmlReadError::DocumentBytesExceeded { .. })
    ));

    let small_tokens = XmlBudgets::builder()
        .maximum_document_bytes(xml.len())
        .maximum_depth(8)
        .maximum_tokens(2)
        .maximum_attributes_per_element(8)
        .maximum_attribute_count(8)
        .maximum_attribute_bytes(64)
        .maximum_namespace_declarations_per_element(4)
        .maximum_namespace_declaration_count(4)
        .maximum_namespace_bytes(64)
        .maximum_text_bytes(64)
        .build()
        .expect("test задаёт complete budgets");
    assert!(matches!(
        collect_events(xml, small_tokens),
        Err(XmlReadError::TokensExceeded { .. })
    ));

    let small_attribute_bytes = XmlBudgets::builder()
        .maximum_document_bytes(xml.len())
        .maximum_depth(8)
        .maximum_tokens(32)
        .maximum_attributes_per_element(8)
        .maximum_attribute_count(8)
        .maximum_attribute_bytes(3)
        .maximum_namespace_declarations_per_element(4)
        .maximum_namespace_declaration_count(4)
        .maximum_namespace_bytes(64)
        .maximum_text_bytes(64)
        .build()
        .expect("test задаёт complete budgets");
    assert!(matches!(
        collect_events(xml, small_attribute_bytes),
        Err(XmlReadError::AttributeBytesExceeded { .. })
    ));

    let small_text = XmlBudgets::builder()
        .maximum_document_bytes(xml.len())
        .maximum_depth(8)
        .maximum_tokens(32)
        .maximum_attributes_per_element(8)
        .maximum_attribute_count(8)
        .maximum_attribute_bytes(64)
        .maximum_namespace_declarations_per_element(4)
        .maximum_namespace_declaration_count(4)
        .maximum_namespace_bytes(64)
        .maximum_text_bytes(3)
        .build()
        .expect("test задаёт complete budgets");
    assert_eq!(
        collect_events(xml, small_text).expect_err("четыре text bytes превышают limit"),
        XmlReadError::TextBytesExceeded {
            observed: 4,
            maximum: 3,
        }
    );

    collect_events(xml, generous_budgets()).expect("те же bytes проходят generous policy");
}

#[test]
fn malformed_namespace_duplicate_attribute_and_document_shape_fail_closed() {
    assert_eq!(
        collect_events(br#"<p:root/>"#, generous_budgets()).expect_err("unbound prefix rejected"),
        XmlReadError::InvalidNamespace
    );
    assert_eq!(
        collect_events(br#"<root a="1" a="2"/>"#, generous_budgets())
            .expect_err("duplicate attribute rejected"),
        XmlReadError::MalformedAttribute
    );
    assert_eq!(
        collect_events(
            br#"<root xmlns:left="urn:same" xmlns:right="urn:same" left:id="1" right:id="2"/>"#,
            generous_budgets(),
        )
        .expect_err("namespace aliases must not bypass expanded-name duplicate checks"),
        XmlReadError::MalformedAttribute
    );
    assert_eq!(
        collect_events(br#"<first/><second/>"#, generous_budgets())
            .expect_err("multiple roots rejected"),
        XmlReadError::MultipleRootElements
    );
    assert_eq!(
        collect_events(b"plain text", generous_budgets()).expect_err("text outside root rejected"),
        XmlReadError::TextOutsideRoot
    );
    assert_eq!(
        collect_events(b" \n\t", generous_budgets())
            .expect_err("whitespace-only input has no root"),
        XmlReadError::MissingRootElement
    );
}

#[test]
fn complete_builder_has_no_hidden_defaults_and_failed_reader_is_fused() {
    let missing = XmlBudgets::builder()
        .build()
        .expect_err("пустой builder не подставляет policy");
    assert_eq!(missing.field(), XmlBudgetKind::DocumentBytes);

    let mut reader = BoundedXmlReader::new(b"<root>&custom;</root>", generous_budgets())
        .expect("byte budget допускает reader");
    assert!(matches!(
        reader.next_event(),
        Ok(Some(XmlEvent::StartElement(_)))
    ));
    let first_error = reader
        .next_event()
        .expect_err("custom entity terminal-ит reader");
    let repeated_error = reader
        .next_event()
        .expect_err("reader не продолжает parsing после failure");
    assert_eq!(first_error, XmlReadError::CustomEntityForbidden);
    assert_eq!(repeated_error, first_error);
}

#[test]
fn unsupported_declared_encoding_is_rejected_without_guessing() {
    let xml = br#"<?xml version="1.0" encoding="windows-1251"?><root/>"#;
    assert_eq!(
        collect_events(xml, generous_budgets()).expect_err("legacy encoding не угадывается"),
        XmlReadError::UnsupportedEncoding
    );
}

#[test]
fn xml_1_1_and_declaration_after_any_content_are_rejected_explicitly() {
    let xml_1_1 = br#"<?xml version="1.1"?><root/>"#;
    assert_eq!(
        collect_events(xml_1_1, generous_budgets()).expect_err("partial XML 1.1 support запрещён"),
        XmlReadError::UnsupportedXmlVersion
    );

    let misplaced = br#"<!--prolog comment--><?xml version="1.0"?><root/>"#;
    assert_eq!(
        collect_events(misplaced, generous_budgets())
            .expect_err("declaration после comment уже не является первым construct"),
        XmlReadError::MisplacedXmlDeclaration
    );

    let non_xml_whitespace = "\u{00a0}<root/>".as_bytes();
    assert_eq!(
        collect_events(non_xml_whitespace, generous_budgets())
            .expect_err("NBSP вне root не является XML markup whitespace"),
        XmlReadError::TextOutsideRoot
    );
}

#[test]
fn declaration_duplicate_order_and_standalone_grammar_are_validated() {
    let duplicate_version = br#"<?xml version="1.0" version="1.0"?><root/>"#;
    assert_eq!(
        collect_events(duplicate_version, generous_budgets())
            .expect_err("duplicate declaration attribute rejected"),
        XmlReadError::MalformedAttribute
    );

    let invalid_order = br#"<?xml version="1.0" standalone="yes" encoding="UTF-8"?><root/>"#;
    assert_eq!(
        collect_events(invalid_order, generous_budgets())
            .expect_err("encoding после standalone нарушает declaration grammar"),
        XmlReadError::MalformedAttribute
    );

    let invalid_standalone = br#"<?xml version="1.0" standalone="maybe"?><root/>"#;
    assert_eq!(
        collect_events(invalid_standalone, generous_budgets())
            .expect_err("standalone принимает только yes/no"),
        XmlReadError::MalformedAttribute
    );

    let valid_standalone = br#"<?xml version="1.0" encoding="utf-8" standalone="no"?><root/>"#;
    collect_events(valid_standalone, generous_budgets()).expect("полная XML 1.0 declaration legal");
}

#[test]
fn custom_entity_inside_attribute_is_rejected() {
    let xml = br#"<root title="unsafe &custom; value"/>"#;
    assert_eq!(
        collect_events(xml, generous_budgets())
            .expect_err("custom attribute entity не раскрывается"),
        XmlReadError::CustomEntityForbidden
    );
}
