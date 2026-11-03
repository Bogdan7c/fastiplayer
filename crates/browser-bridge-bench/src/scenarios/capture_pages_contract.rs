//! Договор таблицы `capture` со страницами на диске: страницы известны каталогу, а имя
//! действия страницы (`hook:<имя>`) действительно объявлено в её исходнике. Без этого робот
//! послал бы команду, которую страница не знает, и стенд упал бы только на живом прогоне.

use super::capture::{CAPTURE_PAGES, capture_page_ids};
use super::page_catalog::PAGE_SCENARIOS;

/// Страницы `capture` — известные страницы каталога, а имя действия есть в самой странице
/// (иначе робот пошлёт команду, которую страница не знает, и стенд упадёт только вживую).
#[test]
fn capture_pages_exist_in_the_catalog_and_declare_their_hooks() {
    let mut seen = std::collections::HashSet::new();
    for page in CAPTURE_PAGES {
        assert!(seen.insert(page.id), "duplicate capture page {}", page.id);
        assert!(
            PAGE_SCENARIOS.iter().any(|scenario| scenario.id == page.id),
            "{} is not in the page catalog",
            page.id
        );
        let Some(hook) = page.hook else { continue };
        let source = page_source_with_libs(page.id);
        assert!(
            source.contains(&format!("'{hook}'")),
            "page {} does not declare the hook '{hook}'",
            page.id
        );
    }
}

#[test]
fn capture_page_ids_lists_every_capture_page_including_the_new_variants() {
    let ids = capture_page_ids();
    for expected in [
        "mse-generation-swap-srcobject-null",
        "mse-generation-swap-reload",
        "mse-generation-swap-delayed",
        "cached-descriptors",
    ] {
        assert!(
            ids.contains(&expected),
            "{expected} is missing from capture"
        );
    }
}

/// Общие тела страниц, в которых объявлены хуки (смена поколения, MSE в воркере — 09b).
const PAGE_BODY_LIBS: &[&str] = &["generation-swap-page.js", "worker-page.js"];

/// Текст страницы вместе с общими телами страниц, которые она подключает (хук объявлен там).
fn page_source_with_libs(id: &str) -> String {
    let site = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("site");
    let mut source = std::fs::read_to_string(site.join("pages").join(format!("{id}.html")))
        .unwrap_or_else(|error| panic!("page {id}: {error}"));
    for lib in PAGE_BODY_LIBS {
        if source.contains(lib) {
            source.push_str(
                &std::fs::read_to_string(site.join("lib").join(lib))
                    .unwrap_or_else(|error| panic!("page body lib {lib}: {error}")),
            );
        }
    }
    source
}
