//! Сторож «забытого проброса»: новый метод в трейте без проброса и проверки в обёртке GLES2
//! роняет тест. Список методов берётся из исходника трейта, а список проверенных — из журнала
//! вызовов fake (то, что реально дошло до внутреннего объекта), плюс явный список
//! «намеренно не форвардится».

/// Имена `fn` на верхнем уровне блока трейта, который начинается с `trait_header`.
///
/// Разбор простой, но устойчивый к вложенности: комментарии `//` отрезаются, фигурные скобки
/// считаются (тела дефолтных методов с `{}` не сбивают глубину), `fn` засчитывается только на
/// глубине 1 (прямо в трейте).
pub(super) fn trait_method_names(source: &str, trait_header: &str) -> Vec<String> {
    let without_comments: String = source
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let header_start = without_comments
        .find(trait_header)
        .unwrap_or_else(|| panic!("в исходнике нет `{trait_header}`: поправь сторож в тесте"));
    let block = &without_comments[header_start..];
    let body_start = block.find('{').expect("у трейта есть тело");

    let mut depth = 0usize;
    let mut names = Vec::new();
    let mut previous = ' ';
    let characters: Vec<(usize, char)> = block.char_indices().skip(body_start).collect();
    for (position, character) in characters.iter().copied() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        let at_trait_level = depth == 1;
        if at_trait_level && character == 'f' && previous.is_whitespace() {
            let rest = &block[position..];
            if let Some(after_fn) = rest.strip_prefix("fn ") {
                let name: String = after_fn
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                names.push(name);
            }
        }
        previous = character;
    }
    names
}

/// Проверяет: методы трейта == (вызванные у внутреннего объекта) ∪ (намеренно не форвардимые).
pub(super) fn assert_forwarding_covers_trait(
    trait_header: &str,
    trait_source: &str,
    forwarded_to_inner: &[&'static str],
    intentionally_not_forwarded: &[&str],
) {
    let mut trait_methods = trait_method_names(trait_source, trait_header);
    trait_methods.sort();
    trait_methods.dedup();
    assert!(
        trait_methods.len() >= 5,
        "парсер трейта `{trait_header}` нашёл подозрительно мало методов: {trait_methods:?}"
    );

    let mut checked: Vec<String> = forwarded_to_inner
        .iter()
        .copied()
        .chain(intentionally_not_forwarded.iter().copied())
        .map(str::to_owned)
        .collect();
    checked.sort();
    checked.dedup();

    let missing: Vec<&String> = trait_methods
        .iter()
        .filter(|method| !checked.contains(method))
        .collect();
    assert!(
        missing.is_empty(),
        "в трейте `{trait_header}` есть методы без проброса и проверки в обёртке GLES2: \
         {missing:?}. Добавь проброс в обёртку (src/frame_release) и проверку в \
         wrapper_forwarding.rs"
    );
    let unknown: Vec<&String> = checked
        .iter()
        .filter(|method| !trait_methods.contains(method))
        .collect();
    assert!(
        unknown.is_empty(),
        "тест проверяет методы, которых нет в трейте `{trait_header}`: {unknown:?}"
    );
}

#[test]
fn parser_finds_methods_with_default_bodies_and_ignores_comments() {
    let source = r#"
        pub trait Sample {
            type Output;
            // fn commented_out(&self);
            fn required(&self) -> u8;
            fn defaulted(&self) -> String {
                format!("{}", if true { 1 } else { 2 })
            }
            fn last(&self);
        }
        fn outside_trait() {}
    "#;

    assert_eq!(
        trait_method_names(source, "pub trait Sample"),
        vec!["required", "defaulted", "last"]
    );
}

#[test]
fn parser_reads_the_real_trait_sources() {
    let decoder_methods = trait_method_names(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../video-core/src/decoder_thread.rs"
        )),
        "pub trait VideoDecoderThreadHandle",
    );
    let provider_methods = trait_method_names(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../video-backend-api/src/lib.rs"
        )),
        "pub trait PresentFrameResourceProvider",
    );

    assert!(decoder_methods.contains(&"decoder_activity_snapshot".to_owned()));
    assert!(decoder_methods.contains(&"drain_completed_packet_count".to_owned()));
    assert_eq!(provider_methods.len(), 6, "{provider_methods:?}");
    assert!(provider_methods.contains(&"try_resource_identity_lookup".to_owned()));
}
