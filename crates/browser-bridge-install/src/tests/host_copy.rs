//! Замена посредника: права до подмены, чистота каталога, прежний файл при сбое.

use std::io;

use super::support::{HOST_BYTES, mode_of, set_mode};
use crate::fs_util::set_file_mode;
use crate::host_copy::{HostReplaceOutcome, replace_with_mode};

fn names_in(directory: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn overwriting_a_non_executable_host_leaves_new_bytes_with_mode_0755_and_no_temp_files() {
    let directory = tempfile::tempdir().expect("dir");
    let target = directory.path().join("host");
    std::fs::write(&target, b"old").expect("old host");
    set_mode(&target, 0o600);

    let outcome = replace_with_mode(&target, HOST_BYTES, 0o755, set_file_mode).expect("replace");

    assert_eq!(outcome, HostReplaceOutcome::Durable);
    assert_eq!(std::fs::read(&target).expect("read"), HOST_BYTES);
    assert_eq!(mode_of(&target), 0o755);
    assert_eq!(names_in(directory.path()), vec!["host".to_owned()]);
}

#[test]
fn failing_chmod_keeps_the_previous_host_untouched_and_removes_the_temp_file() {
    let directory = tempfile::tempdir().expect("dir");
    let target = directory.path().join("host");
    std::fs::write(&target, b"previous").expect("previous host");
    set_mode(&target, 0o755);

    let result = replace_with_mode(&target, HOST_BYTES, 0o755, |_path, _mode| {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    });

    assert_eq!(
        result.expect_err("chmod failed").kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(std::fs::read(&target).expect("read"), b"previous");
    assert_eq!(mode_of(&target), 0o755);
    assert_eq!(names_in(directory.path()), vec!["host".to_owned()]);
}
