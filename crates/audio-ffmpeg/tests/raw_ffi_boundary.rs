// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Граница crate-а: `ffmpeg_sys_next`, типы `AV*` и `unsafe` живут только в
//! `src/ffi/`. Остальные модули и публичный API работают с нейтральными
//! типами `audio-core` (тот же договор, что у `video-ffmpeg`).

use std::fs;
use std::path::{Path, PathBuf};

/// Что не должно встречаться в исходниках вне `src/ffi/`.
const FORBIDDEN_OUTSIDE_FFI: [&str; 6] = [
    "ffmpeg_sys_next",
    "AVPacket",
    "AVFrame",
    "AVCodecContext",
    "*mut AV",
    "unsafe",
];

#[test]
fn ffmpeg_types_and_unsafe_stay_inside_the_ffi_module() {
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();
    collect_violations(&source_root, &source_root, &mut violations);
    assert!(
        violations.is_empty(),
        "FFmpeg types or unsafe outside src/ffi:\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_ffi_module_exists_and_holds_the_raw_calls() {
    // Без этой проверки тест выше прошёл бы и при пустом/переименованном модуле.
    let codec_context = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/ffi/codec_context.rs");
    let source = fs::read_to_string(codec_context).expect("ffi/codec_context.rs exists");
    assert!(source.contains("ffmpeg_sys_next::avcodec_send_packet"));
}

fn collect_violations(source_root: &Path, directory: &Path, violations: &mut Vec<String>) {
    for entry in fs::read_dir(directory).expect("source directory is readable") {
        let path = entry.expect("directory entry is readable").path();
        if path.is_dir() {
            if !path.ends_with("ffi") {
                collect_violations(source_root, &path, violations);
            }
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("source file is readable");
        let relative_path = path.strip_prefix(source_root).expect("under src").display();
        for (line_index, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            if let Some(fragment) = FORBIDDEN_OUTSIDE_FFI
                .iter()
                .find(|fragment| code.contains(*fragment))
            {
                violations.push(format!(
                    "{relative_path}:{}: `{fragment}` in {}",
                    line_index + 1,
                    line.trim()
                ));
            }
        }
    }
}
