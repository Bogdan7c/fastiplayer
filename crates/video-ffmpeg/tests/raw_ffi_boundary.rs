use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn public_non_ffi_modules_do_not_expose_raw_ffmpeg_types() {
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();

    collect_public_raw_ffmpeg_exposures(&source_root, &source_root, &mut violations);

    assert!(
        violations.is_empty(),
        "raw FFmpeg pointer/type exposure outside ffi boundary:\n{}",
        violations.join("\n")
    );
}

fn collect_public_raw_ffmpeg_exposures(
    source_root: &Path,
    current_path: &Path,
    violations: &mut Vec<String>,
) {
    let entries = fs::read_dir(current_path).expect("source directory should be readable");

    for entry in entries {
        let entry = entry.expect("source directory entry should be readable");
        let path = entry.path();

        if path.is_dir() {
            if path.ends_with("ffi") {
                continue;
            }

            collect_public_raw_ffmpeg_exposures(source_root, &path, violations);
            continue;
        }

        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }

        inspect_public_lines(source_root, &path, violations);
    }
}

fn inspect_public_lines(source_root: &Path, path: &Path, violations: &mut Vec<String>) {
    let source = fs::read_to_string(path).expect("source file should be readable");
    let relative_path = path
        .strip_prefix(source_root)
        .expect("path should be under source root")
        .display();

    for (line_index, source_line) in source.lines().enumerate() {
        let trimmed_line = source_line.trim_start();

        if !trimmed_line.starts_with("pub ") && !trimmed_line.starts_with("pub(") {
            continue;
        }

        if exposes_raw_ffmpeg_type(trimmed_line) {
            violations.push(format!(
                "{relative_path}:{}: {trimmed_line}",
                line_index + 1
            ));
        }
    }
}

fn exposes_raw_ffmpeg_type(source_line: &str) -> bool {
    let forbidden_fragments = [
        "ffmpeg_sys_next",
        "AVPacket",
        "AVFrame",
        "AVCodecContext",
        "*mut AV",
        "*const AV",
    ];

    forbidden_fragments
        .iter()
        .any(|forbidden_fragment| source_line.contains(forbidden_fragment))
}
