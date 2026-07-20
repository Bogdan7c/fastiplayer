use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use playlist_io::{
    ExpandedLocalPlaylistDocument, ExpandedLocalPlaylistEntry, LocalPlaylistExpansion,
    LocalPlaylistExpansionCancellation, LocalPlaylistExpansionIssueKind,
    LocalPlaylistExpansionLimits, LocalPlaylistExpansionRequest, M3uParserLimits, XspfParserLimits,
    expand_local_playlist,
};

/// Process-local suffix исключает collisions между параллельными tests.
static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

/// RAII temp directory принадлежит ровно одному focused test.
struct TestDirectory {
    /// Exact native path.
    path: PathBuf,
}

impl TestDirectory {
    /// Создаёт уникальную absolute directory без external test dependency.
    fn new(test_name: &str) -> Self {
        let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rustiplayer-playlist-io-{test_name}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test directory должна создаваться");
        Self { path }
    }

    /// Возвращает child path.
    fn join(&self, child: impl AsRef<Path>) -> PathBuf {
        self.path.join(child)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("test directory должна удаляться");
    }
}

/// Запускает production filesystem boundary с заданными aggregate limits.
fn expand(root: &Path, limits: LocalPlaylistExpansionLimits) -> LocalPlaylistExpansion {
    let cancellation = LocalPlaylistExpansionCancellation::new();
    expand_local_playlist(LocalPlaylistExpansionRequest::new(
        root,
        limits,
        M3uParserLimits::default(),
        XspfParserLimits::default(),
        &cancellation,
    ))
    .expect("root format/path должны быть valid")
}

/// Собирает exact local M3U leaf paths в deterministic DFS order.
fn collect_m3u_leaf_paths(document: &ExpandedLocalPlaylistDocument, output: &mut Vec<PathBuf>) {
    for entry in document.depth_first_entries() {
        match entry {
            ExpandedLocalPlaylistEntry::M3uItem(import_draft) => {
                let path = import_draft
                    .reopen_locator()
                    .expose_local_for_reopen()
                    .expect("test leaf должен быть local")
                    .expose_native_path_for_open()
                    .expect("test leaf должен быть native");
                output.push(path.to_path_buf());
            }
            ExpandedLocalPlaylistEntry::XspfTrack(_)
            | ExpandedLocalPlaylistEntry::IncludedDocument(_)
            | ExpandedLocalPlaylistEntry::UnexpandedInclude(_) => {}
        }
    }
}

/// Возвращает все retained issue kinds.
fn issue_kinds(expansion: &LocalPlaylistExpansion) -> Vec<LocalPlaylistExpansionIssueKind> {
    expansion
        .issues()
        .iter()
        .map(|issue| issue.kind())
        .collect()
}

#[test]
fn self_cycle_is_cut_by_active_canonical_identity() {
    let directory = TestDirectory::new("self-cycle");
    let root = directory.join("root.m3u");
    fs::write(&root, "root.m3u\nsong.mp3\n").expect("root fixture");

    let expansion = expand(&root, LocalPlaylistExpansionLimits::default());
    let root_document = expansion.root_document().expect("root parsed");

    assert!(matches!(
        root_document.entries().first(),
        Some(ExpandedLocalPlaylistEntry::UnexpandedInclude(_))
    ));
    assert_eq!(expansion.summary().cycle_rejections(), 1);
    assert!(issue_kinds(&expansion).contains(&LocalPlaylistExpansionIssueKind::CycleDetected));
}

#[test]
fn two_document_cycle_is_cut_without_losing_prior_document_boundaries() {
    let directory = TestDirectory::new("two-cycle");
    let first = directory.join("a.m3u");
    let second = directory.join("b.m3u");
    fs::write(&first, "b.m3u\n").expect("first fixture");
    fs::write(&second, "a.m3u\n").expect("second fixture");

    let expansion = expand(&first, LocalPlaylistExpansionLimits::default());
    let first_document = expansion.root_document().expect("first parsed");
    let ExpandedLocalPlaylistEntry::IncludedDocument(second_document) =
        &first_document.entries()[0]
    else {
        panic!("A должен раскрыть B");
    };

    assert!(matches!(
        second_document.entries().first(),
        Some(ExpandedLocalPlaylistEntry::UnexpandedInclude(_))
    ));
    assert_eq!(expansion.summary().documents_attempted(), 2);
    assert_eq!(expansion.summary().cycle_rejections(), 1);
}

#[test]
fn repeated_non_cycle_include_is_expanded_each_time_in_depth_first_order() {
    let directory = TestDirectory::new("repeat");
    let root = directory.join("root.m3u");
    let child = directory.join("child.m3u");
    fs::write(
        &root,
        "before.mp3\nchild.m3u\nmiddle.mp3\nchild.m3u\nafter.mp3\n",
    )
    .expect("root fixture");
    fs::write(&child, "inside.mp3\n").expect("child fixture");

    let expansion = expand(&root, LocalPlaylistExpansionLimits::default());
    let mut leaf_paths = Vec::new();
    collect_m3u_leaf_paths(
        expansion.root_document().expect("root parsed"),
        &mut leaf_paths,
    );

    assert_eq!(
        leaf_paths,
        vec![
            directory.join("before.mp3"),
            directory.join("inside.mp3"),
            directory.join("middle.mp3"),
            directory.join("inside.mp3"),
            directory.join("after.mp3"),
        ]
    );
    assert_eq!(expansion.summary().documents_attempted(), 3);
}

#[test]
fn network_playlist_url_remains_leaf_and_is_never_fetched() {
    let directory = TestDirectory::new("network-leaf");
    let root = directory.join("root.m3u");
    fs::write(
        &root,
        "https://example.invalid/private/nested.m3u?token=secret\n",
    )
    .expect("root fixture");

    let expansion = expand(&root, LocalPlaylistExpansionLimits::default());
    let root_document = expansion.root_document().expect("root parsed");

    assert!(matches!(
        root_document.entries().first(),
        Some(ExpandedLocalPlaylistEntry::M3uItem(_))
    ));
    assert_eq!(expansion.summary().documents_attempted(), 1);
}

#[test]
fn xspf_single_local_playlist_location_expands_but_network_alternative_does_not() {
    let directory = TestDirectory::new("xspf");
    let root = directory.join("root.xspf");
    let child = directory.join("child.m3u");
    fs::write(&child, "inside.mp3\n").expect("child fixture");
    fs::write(
        &root,
        r#"<playlist xmlns="http://xspf.org/ns/0/" version="1">
  <trackList>
    <track><location>child.m3u</location></track>
    <track><location>https://example.invalid/nested.xspf</location></track>
  </trackList>
</playlist>"#,
    )
    .expect("XSPF fixture");

    let expansion = expand(&root, LocalPlaylistExpansionLimits::default());
    let root_document = expansion.root_document().expect("XSPF root parsed");

    assert!(matches!(
        root_document.entries().first(),
        Some(ExpandedLocalPlaylistEntry::IncludedDocument(_))
    ));
    assert!(matches!(
        root_document.entries().get(1),
        Some(ExpandedLocalPlaylistEntry::XspfTrack(_))
    ));
    assert_eq!(expansion.summary().documents_attempted(), 2);
}

#[cfg(unix)]
#[test]
fn symlink_alias_participates_only_in_cycle_detection() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new("symlink-cycle");
    let root = directory.join("root.m3u");
    let alias = directory.join("alias.m3u");
    fs::write(&root, "alias.m3u\n").expect("root fixture");
    symlink(&root, &alias).expect("symlink fixture");

    let expansion = expand(&root, LocalPlaylistExpansionLimits::default());
    let root_document = expansion.root_document().expect("root parsed");

    assert!(matches!(
        root_document.entries().first(),
        Some(ExpandedLocalPlaylistEntry::UnexpandedInclude(_))
    ));
    assert_eq!(
        root_document.source_path(),
        root,
        "stored locator не заменяется canonical path-ом"
    );
    assert_eq!(expansion.summary().cycle_rejections(), 1);
}

#[cfg(unix)]
#[test]
fn dangling_symlink_reports_canonicalization_failure() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new("dangling");
    let root = directory.join("root.m3u");
    let dangling = directory.join("dangling.m3u");
    fs::write(&root, "dangling.m3u\n").expect("root fixture");
    symlink(directory.join("missing.m3u"), &dangling).expect("dangling symlink fixture");

    let expansion = expand(&root, LocalPlaylistExpansionLimits::default());

    assert!(
        issue_kinds(&expansion).contains(&LocalPlaylistExpansionIssueKind::CanonicalizationFailed)
    );
}

#[cfg(unix)]
#[test]
fn non_utf_base_is_preserved_without_lossy_identity() {
    use std::os::unix::ffi::OsStringExt;

    let directory = TestDirectory::new("non-utf");
    let mut filename_bytes = b"list-".to_vec();
    filename_bytes.push(0xff);
    filename_bytes.extend_from_slice(b".m3u");
    let root = directory.join(OsString::from_vec(filename_bytes));
    fs::write(&root, "song.mp3\n").expect("non-UTF root fixture");

    let expansion = expand(&root, LocalPlaylistExpansionLimits::default());
    let root_document = expansion.root_document().expect("root parsed");
    let mut leaf_paths = Vec::new();
    collect_m3u_leaf_paths(root_document, &mut leaf_paths);

    assert_eq!(root_document.source_path(), root);
    assert_eq!(leaf_paths, vec![directory.join("song.mp3")]);
}

#[cfg(unix)]
#[test]
fn non_utf_xspf_document_base_resolves_relative_location_reversibly() {
    use std::os::unix::ffi::OsStringExt;

    let directory = TestDirectory::new("non-utf-xspf");
    let mut directory_name_bytes = b"lists-".to_vec();
    directory_name_bytes.push(0xfe);
    let non_utf_directory = directory.join(OsString::from_vec(directory_name_bytes));
    fs::create_dir(&non_utf_directory).expect("non-UTF child directory");
    let root = non_utf_directory.join("main.xspf");
    fs::write(
        &root,
        r#"<playlist xmlns="http://xspf.org/ns/0/" version="1">
  <trackList><track><location>song.mp3</location></track></trackList>
</playlist>"#,
    )
    .expect("non-UTF XSPF fixture");

    let expansion = expand(&root, LocalPlaylistExpansionLimits::default());
    let root_document = expansion.root_document().expect("XSPF root parsed");
    let ExpandedLocalPlaylistEntry::XspfTrack(track) = &root_document.entries()[0] else {
        panic!("relative media location должна остаться XSPF leaf");
    };
    let resolved_location = track.location_candidates()[0].expose_uri_for_admission();

    assert_eq!(root_document.source_path(), root);
    assert!(resolved_location.ends_with("/song.mp3"));
    assert!(
        resolved_location.contains("%FE"),
        "non-UTF base component должен сохраниться percent-encoded"
    );
}

#[test]
fn aggregate_budgets_keep_lossless_truncation_and_diagnostic_summaries() {
    let directory = TestDirectory::new("budgets");
    let root = directory.join("root.m3u");
    fs::write(&root, "child.m3u\nfirst.mp3\nsecond.mp3\n").expect("root fixture");
    fs::write(directory.join("child.m3u"), "inside.mp3\n").expect("child fixture");
    let limits = LocalPlaylistExpansionLimits::new(
        0,
        1,
        fs::metadata(&root)
            .expect("root metadata")
            .len()
            .try_into()
            .expect("fixture length"),
        1,
        1,
    )
    .expect("valid tight limits");

    let expansion = expand(&root, limits);
    let summary = expansion.summary();

    assert_eq!(summary.depth_truncations(), 1);
    assert_eq!(summary.item_truncations(), 1);
    assert_eq!(summary.retained_items(), 1);
    assert_eq!(summary.total_diagnostics(), 2);
    assert_eq!(summary.omitted_diagnostics(), 1);
    assert_eq!(expansion.issues().len(), 1);
    assert!(summary.was_truncated());
}

#[test]
fn document_and_byte_budgets_produce_distinct_typed_truncation() {
    let directory = TestDirectory::new("document-byte-budgets");
    let root = directory.join("root.m3u");
    let first_child = directory.join("first.m3u");
    let second_child = directory.join("second.m3u");
    fs::write(&root, "first.m3u\nsecond.m3u\n").expect("root fixture");
    fs::write(&first_child, "first.mp3\n").expect("first child fixture");
    fs::write(&second_child, "second.mp3\n").expect("second child fixture");

    let document_limited = expand(
        &root,
        LocalPlaylistExpansionLimits::new(8, 1, 1024, 16, 16)
            .expect("valid document-limited profile"),
    );
    assert_eq!(document_limited.summary().document_truncations(), 2);
    assert_eq!(document_limited.summary().byte_truncations(), 0);
    assert!(
        issue_kinds(&document_limited)
            .contains(&LocalPlaylistExpansionIssueKind::DocumentBudgetExceeded)
    );

    let root_bytes = fs::metadata(&root)
        .expect("root metadata")
        .len()
        .try_into()
        .expect("fixture length");
    let byte_limited = expand(
        &root,
        LocalPlaylistExpansionLimits::new(8, 3, root_bytes, 16, 16)
            .expect("valid byte-limited profile"),
    );
    assert_eq!(byte_limited.summary().document_truncations(), 0);
    assert_eq!(byte_limited.summary().byte_truncations(), 2);
    assert!(
        issue_kinds(&byte_limited).contains(&LocalPlaylistExpansionIssueKind::ByteBudgetExceeded)
    );
}

#[test]
fn per_format_document_cap_stops_read_before_large_aggregate_budget() {
    let directory = TestDirectory::new("format-byte-cap");
    let root = directory.join("root.m3u");
    fs::write(&root, "first.mp3\nsecond.mp3\n").expect("root fixture");
    let cancellation = LocalPlaylistExpansionCancellation::new();
    let m3u_limits = M3uParserLimits::new(8, 64, 16, 16).expect("valid tight M3U document limit");
    let expansion = expand_local_playlist(LocalPlaylistExpansionRequest::new(
        &root,
        LocalPlaylistExpansionLimits::new(8, 8, usize::MAX, 16, 16)
            .expect("large aggregate limit остаётся valid"),
        m3u_limits,
        XspfParserLimits::default(),
        &cancellation,
    ))
    .expect("request valid");

    assert!(expansion.root_document().is_none());
    assert_eq!(expansion.summary().byte_truncations(), 0);
    assert!(issue_kinds(&expansion).contains(&LocalPlaylistExpansionIssueKind::M3uParseFailed));
}

#[test]
fn local_hls_is_typed_unsupported_and_segments_never_become_items() {
    let directory = TestDirectory::new("local-hls");
    let root = directory.join("root.m3u8");
    fs::write(
        &root,
        "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nsegment.ts\n#EXT-X-ENDLIST\n",
    )
    .expect("HLS fixture");

    let expansion = expand(&root, LocalPlaylistExpansionLimits::default());

    assert!(expansion.root_document().is_none());
    assert_eq!(expansion.summary().retained_items(), 0);
    assert!(
        issue_kinds(&expansion).contains(&LocalPlaylistExpansionIssueKind::LocalHlsUnsupported)
    );
}
