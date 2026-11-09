//! Договоры сценария `page-subtitles` со стендом: страницы, хуки и файлы субтитров существуют.

use std::path::{Path, PathBuf};

use super::*;
use crate::scenarios::page_catalog::PAGE_SCENARIOS;

fn site_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("site")
}

fn page_source(page_id: &str) -> String {
    std::fs::read_to_string(site_dir().join(format!("pages/{page_id}.html"))).expect("page file")
}

#[test]
fn every_page_of_the_command_is_in_the_catalog_and_on_disk() {
    for id in page_ids() {
        assert!(
            PAGE_SCENARIOS.iter().any(|scenario| scenario.id == id),
            "{id} is not in PAGE_SCENARIOS"
        );
        assert!(page_source(id).contains(&format!("runScenario('{id}'")));
    }
}

/// Команду, которую шлёт робот, страница обязана объявить, иначе стенд упал бы только вживую.
#[test]
fn the_hook_the_robot_sends_is_declared_by_the_growing_page() {
    let hook = ADD_CUES_COMMAND
        .strip_prefix("hook:")
        .expect("hook command");
    assert!(page_source("subtitles-text-track").contains(&format!("'{hook}'")));
}

/// Страницы берут субтитры из `site/subtitles/`; файлы есть и покрывают 300 с (слой субтитров
/// обязан быть на кадрах в любой момент передачи).
#[test]
fn subtitle_files_exist_and_cover_five_minutes() {
    for file in [
        "english.vtt",
        "ukrainian.vtt",
        "fetched.vtt",
        "xhr.srt",
        "xhr.json3",
    ] {
        let text = std::fs::read_to_string(site_dir().join("subtitles").join(file)).expect(file);
        let marker = if file.ends_with("json3") {
            "\"tStartMs\": 298000"
        } else {
            "00:04:58"
        };
        assert!(text.contains(marker), "{file} does not reach 298 s");
    }
    let sources: String = ["subtitles-track", "subtitles-fetch", "subtitles-xhr"]
        .iter()
        .map(|id| page_source(id))
        .collect();
    for file in [
        "english.vtt",
        "ukrainian.vtt",
        "fetched.vtt",
        "xhr.srt",
        "xhr.json3",
    ] {
        assert!(sources.contains(file), "no page uses {file}");
    }
}

#[test]
fn the_xhr_page_downloads_srt_before_json3_so_json3_is_the_selected_one() {
    let source = page_source("subtitles-xhr");
    let srt = source.find("xhr.srt").expect("srt");
    let json3 = source.find("xhr.json3").expect("json3");
    assert!(srt < json3);
    let expected = PAGES
        .iter()
        .find(|page| page.id == "subtitles-xhr")
        .expect("page");
    assert_eq!(expected.expectation.selected.format, "json3");
    assert!(
        expected
            .expectation
            .documents
            .iter()
            .any(|d| d.format_hint == "json3" && d.page_showing)
    );
    assert!(
        expected
            .expectation
            .documents
            .iter()
            .any(|d| d.format_hint == "srt" && !d.page_showing)
    );
}

#[test]
fn a_filter_selects_pages_by_id() {
    let only = PageFilter::only(["subtitles-xhr".to_owned()]);
    let admitted: Vec<&str> = page_ids()
        .into_iter()
        .filter(|id| only.admits(id))
        .collect();
    assert_eq!(admitted, ["subtitles-xhr"]);
}
