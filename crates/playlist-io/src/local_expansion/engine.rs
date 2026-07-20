//! Filesystem traversal и aggregate accounting recursive local expansion.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use playlist_core::PlaylistSingleImportDraft;
use url::Url;

use crate::{
    GenericM3uPreview, M3uDeclaredFormat, M3uDocument, M3uParseRequest, M3uParserLimits,
    PlaylistDocumentSource, XspfParseRequest, XspfParserLimits, XspfPlaylist, XspfTrack,
    parse_m3u_document, parse_xspf_document,
};

use super::model::{
    ExpandedLocalPlaylistDocument, ExpandedLocalPlaylistEntry, LocalPlaylistDocumentFormat,
    LocalPlaylistExpansion, LocalPlaylistExpansionCancellation, LocalPlaylistExpansionIssue,
    LocalPlaylistExpansionIssueKind, LocalPlaylistExpansionLimits,
    LocalPlaylistExpansionStartError, LocalPlaylistExpansionSummary,
    UnexpandedLocalPlaylistInclude,
};

/// Complete request явно называет root, aggregate/parser budgets и cancellation.
pub struct LocalPlaylistExpansionRequest<'request> {
    /// Reversible absolute root locator.
    root_document_path: PathBuf,
    /// Aggregate recursive budgets.
    expansion_limits: LocalPlaylistExpansionLimits,
    /// Per-document generic M3U/HLS budgets.
    m3u_limits: M3uParserLimits,
    /// Per-document hardened XSPF/XML budgets.
    xspf_limits: XspfParserLimits,
    /// Per-request cancellation state.
    cancellation: &'request LocalPlaylistExpansionCancellation,
}

impl<'request> LocalPlaylistExpansionRequest<'request> {
    /// Собирает self-documenting local expansion intent.
    pub fn new(
        root_document_path: impl Into<PathBuf>,
        expansion_limits: LocalPlaylistExpansionLimits,
        m3u_limits: M3uParserLimits,
        xspf_limits: XspfParserLimits,
        cancellation: &'request LocalPlaylistExpansionCancellation,
    ) -> Self {
        Self {
            root_document_path: root_document_path.into(),
            expansion_limits,
            m3u_limits,
            xspf_limits,
            cancellation,
        }
    }
}

/// Выполняет deterministic depth-first local-only expansion.
pub fn expand_local_playlist(
    request: LocalPlaylistExpansionRequest<'_>,
) -> Result<LocalPlaylistExpansion, LocalPlaylistExpansionStartError> {
    let root_format = document_format(&request.root_document_path)
        .ok_or(LocalPlaylistExpansionStartError::UnsupportedRootFormat)?;
    if !request.root_document_path.is_absolute() {
        return Err(LocalPlaylistExpansionStartError::RootPathMustBeAbsolute);
    }

    let mut reader = FileSystemDocumentReader;
    expand_with_reader(request, root_format, &mut reader)
}

/// Internal reader seam делает cancellation/accounting tests deterministic.
trait LocalDocumentReader {
    /// Возвращает transient canonical identity только для active-stack check.
    fn canonicalize(&mut self, path: &Path) -> io::Result<PathBuf>;

    /// Читает не больше remaining budget плюс один sentinel byte.
    fn read_bounded(
        &mut self,
        path: &Path,
        maximum_bytes: usize,
    ) -> io::Result<BoundedDocumentRead>;
}

/// Production std filesystem adapter без network access.
struct FileSystemDocumentReader;

impl LocalDocumentReader for FileSystemDocumentReader {
    fn canonicalize(&mut self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    fn read_bounded(
        &mut self,
        path: &Path,
        maximum_bytes: usize,
    ) -> io::Result<BoundedDocumentRead> {
        let file = File::open(path)?;
        let sentinel_limit = maximum_bytes.saturating_add(1);
        let read_limit = u64::try_from(sentinel_limit).unwrap_or(u64::MAX);
        let mut bounded_reader = file.take(read_limit);
        let mut document_bytes = Vec::with_capacity(maximum_bytes.min(64 * 1024));
        bounded_reader.read_to_end(&mut document_bytes)?;

        if document_bytes.len() > maximum_bytes {
            return Ok(BoundedDocumentRead::LimitExceeded);
        }
        Ok(BoundedDocumentRead::Complete(document_bytes))
    }
}

/// Bounded read никогда не передаёт parser-у partial prefix.
enum BoundedDocumentRead {
    /// Полный документ помещается в remaining budget.
    Complete(Vec<u8>),
    /// Sentinel доказал, что complete document не помещается.
    LimitExceeded,
}

/// Один mutable owner всех aggregate counters и active canonical identities.
struct ExpansionContext<'request, 'reader, Reader> {
    /// Immutable aggregate profile.
    limits: LocalPlaylistExpansionLimits,
    /// Immutable per-document M3U profile.
    m3u_limits: M3uParserLimits,
    /// Immutable per-document XSPF/XML profile.
    xspf_limits: XspfParserLimits,
    /// Cancellation проверяется только между documents.
    cancellation: &'request LocalPlaylistExpansionCancellation,
    /// Filesystem seam.
    reader: &'reader mut Reader,
    /// Только текущая DFS ancestry, не global dedup.
    active_canonical_paths: HashSet<PathBuf>,
    /// Bounded safe details.
    issues: Vec<LocalPlaylistExpansionIssue>,
    /// Lossless accounting.
    summary: LocalPlaylistExpansionSummary,
}

/// Результат попытки раскрыть один document include.
enum DocumentAttempt {
    /// Include раскрыт и сохраняет document boundary.
    Expanded(ExpandedLocalPlaylistDocument),
    /// Typed issue уже записан; caller сохраняет original include placeholder.
    Unexpanded,
}

/// Запускает engine через production или focused fake reader.
fn expand_with_reader<Reader: LocalDocumentReader>(
    request: LocalPlaylistExpansionRequest<'_>,
    root_format: LocalPlaylistDocumentFormat,
    reader: &mut Reader,
) -> Result<LocalPlaylistExpansion, LocalPlaylistExpansionStartError> {
    let mut context = ExpansionContext {
        limits: request.expansion_limits,
        m3u_limits: request.m3u_limits,
        xspf_limits: request.xspf_limits,
        cancellation: request.cancellation,
        reader,
        active_canonical_paths: HashSet::new(),
        issues: Vec::new(),
        summary: LocalPlaylistExpansionSummary::default(),
    };

    let root_document = match context.expand_document(&request.root_document_path, root_format, 0) {
        DocumentAttempt::Expanded(document) => Some(document),
        DocumentAttempt::Unexpanded => None,
    };

    Ok(LocalPlaylistExpansion::new(
        root_document,
        context.issues,
        context.summary,
    ))
}

impl<Reader: LocalDocumentReader> ExpansionContext<'_, '_, Reader> {
    /// Раскрывает один document после cancellation/budget/cycle admission.
    fn expand_document(
        &mut self,
        source_path: &Path,
        format: LocalPlaylistDocumentFormat,
        depth: usize,
    ) -> DocumentAttempt {
        if self.cancellation.is_cancelled() {
            self.summary.cancelled = true;
            return DocumentAttempt::Unexpanded;
        }
        if depth > self.limits.maximum_depth() {
            self.summary.depth_truncations += 1;
            self.record_issue(LocalPlaylistExpansionIssueKind::DepthBudgetExceeded, depth);
            return DocumentAttempt::Unexpanded;
        }
        if self.summary.documents_attempted >= self.limits.maximum_documents() {
            self.summary.document_truncations += 1;
            self.record_issue(
                LocalPlaylistExpansionIssueKind::DocumentBudgetExceeded,
                depth,
            );
            return DocumentAttempt::Unexpanded;
        }

        let canonical_path = match self.reader.canonicalize(source_path) {
            Ok(canonical_path) => canonical_path,
            Err(_) => {
                self.record_issue(
                    LocalPlaylistExpansionIssueKind::CanonicalizationFailed,
                    depth,
                );
                return DocumentAttempt::Unexpanded;
            }
        };
        if self.active_canonical_paths.contains(&canonical_path) {
            self.summary.cycle_rejections += 1;
            self.record_issue(LocalPlaylistExpansionIssueKind::CycleDetected, depth);
            return DocumentAttempt::Unexpanded;
        }

        self.summary.documents_attempted += 1;
        let remaining_bytes = self
            .limits
            .maximum_total_bytes()
            .saturating_sub(self.summary.document_bytes_read);
        let format_document_limit = self.maximum_document_bytes(format);
        let read_limit = remaining_bytes.min(format_document_limit);
        let document_bytes = match self.reader.read_bounded(source_path, read_limit) {
            Ok(BoundedDocumentRead::Complete(document_bytes)) => document_bytes,
            Ok(BoundedDocumentRead::LimitExceeded) => {
                if remaining_bytes <= format_document_limit {
                    self.summary.byte_truncations += 1;
                    self.record_issue(LocalPlaylistExpansionIssueKind::ByteBudgetExceeded, depth);
                } else {
                    let parser_issue = match format {
                        LocalPlaylistDocumentFormat::M3u | LocalPlaylistDocumentFormat::M3u8 => {
                            LocalPlaylistExpansionIssueKind::M3uParseFailed
                        }
                        LocalPlaylistDocumentFormat::Xspf => {
                            LocalPlaylistExpansionIssueKind::XspfParseFailed
                        }
                    };
                    self.record_issue(parser_issue, depth);
                }
                return DocumentAttempt::Unexpanded;
            }
            Err(_) => {
                self.record_issue(LocalPlaylistExpansionIssueKind::DocumentReadFailed, depth);
                return DocumentAttempt::Unexpanded;
            }
        };
        self.summary.document_bytes_read = self
            .summary
            .document_bytes_read
            .saturating_add(document_bytes.len());

        self.active_canonical_paths.insert(canonical_path.clone());
        let expanded_document = match format {
            LocalPlaylistDocumentFormat::M3u | LocalPlaylistDocumentFormat::M3u8 => {
                self.expand_m3u(source_path, format, &document_bytes, depth)
            }
            LocalPlaylistDocumentFormat::Xspf => {
                self.expand_xspf(source_path, &document_bytes, depth)
            }
        };
        self.active_canonical_paths.remove(&canonical_path);
        expanded_document
    }

    /// Возвращает format-owned hard document cap до parser allocation.
    fn maximum_document_bytes(&self, format: LocalPlaylistDocumentFormat) -> usize {
        match format {
            LocalPlaylistDocumentFormat::M3u | LocalPlaylistDocumentFormat::M3u8 => {
                self.m3u_limits.max_document_bytes()
            }
            LocalPlaylistDocumentFormat::Xspf => {
                self.xspf_limits.xml_budgets().maximum_document_bytes()
            }
        }
    }

    /// Парсит generic/HLS M3U и раскрывает local playlist draft-ы в source order.
    fn expand_m3u(
        &mut self,
        source_path: &Path,
        format: LocalPlaylistDocumentFormat,
        document_bytes: &[u8],
        depth: usize,
    ) -> DocumentAttempt {
        let declared_format = match format {
            LocalPlaylistDocumentFormat::M3u => M3uDeclaredFormat::M3u,
            LocalPlaylistDocumentFormat::M3u8 => M3uDeclaredFormat::M3u8,
            LocalPlaylistDocumentFormat::Xspf => {
                unreachable!("XSPF не проходит через M3U parser")
            }
        };
        let parsed_document = match parse_m3u_document(M3uParseRequest::new(
            document_bytes,
            PlaylistDocumentSource::local(source_path),
            declared_format,
            self.m3u_limits,
        )) {
            Ok(parsed_document) => parsed_document,
            Err(_) => {
                self.record_issue(LocalPlaylistExpansionIssueKind::M3uParseFailed, depth);
                return DocumentAttempt::Unexpanded;
            }
        };

        match parsed_document {
            M3uDocument::Generic(preview) => DocumentAttempt::Expanded(self.expand_generic_m3u(
                source_path,
                format,
                preview,
                depth,
            )),
            M3uDocument::LocalHlsManifestUnsupported(_) => {
                self.record_issue(LocalPlaylistExpansionIssueKind::LocalHlsUnsupported, depth);
                DocumentAttempt::Unexpanded
            }
            M3uDocument::AdaptiveManifestReference(_) => {
                unreachable!("local source не может вернуть network adaptive reference")
            }
        }
    }

    /// Поднимает parser issues и строит M3U document tree.
    fn expand_generic_m3u(
        &mut self,
        source_path: &Path,
        format: LocalPlaylistDocumentFormat,
        preview: GenericM3uPreview,
        depth: usize,
    ) -> ExpandedLocalPlaylistDocument {
        for issue in preview.issues().iter() {
            self.record_issue(
                LocalPlaylistExpansionIssueKind::M3uImportIssue {
                    kind: issue.kind(),
                    line: issue.line(),
                },
                depth,
            );
        }
        self.record_pre_omitted_diagnostics(preview.issues().omitted_issue_count());

        let mut source_complete = !preview.truncated_by_item_limit();
        if preview.truncated_by_item_limit() {
            self.record_issue(
                LocalPlaylistExpansionIssueKind::M3uDocumentItemLimitReached,
                depth,
            );
        }
        let mut entries = Vec::with_capacity(preview.retained_entry_count());

        for entry in preview.entries() {
            if self.summary.cancelled {
                source_complete = false;
                break;
            }
            let import_draft = entry.import_draft().clone();
            let Some((include_path, include_format)) = m3u_include(&import_draft) else {
                if !self.retain_leaf(depth) {
                    source_complete = false;
                    break;
                }
                entries.push(ExpandedLocalPlaylistEntry::M3uItem(Box::new(import_draft)));
                continue;
            };

            match self.expand_document(include_path, include_format, depth.saturating_add(1)) {
                DocumentAttempt::Expanded(document) => {
                    entries.push(ExpandedLocalPlaylistEntry::IncludedDocument(Box::new(
                        document,
                    )));
                }
                DocumentAttempt::Unexpanded => {
                    entries.push(ExpandedLocalPlaylistEntry::UnexpandedInclude(
                        UnexpandedLocalPlaylistInclude::M3uItem(Box::new(import_draft)),
                    ));
                    if self.summary.cancelled {
                        source_complete = false;
                        break;
                    }
                }
            }
        }

        ExpandedLocalPlaylistDocument::new(
            source_path.to_path_buf(),
            format,
            entries,
            Vec::new(),
            source_complete,
        )
    }

    /// Парсит XSPF и раскрывает только unambiguous single local playlist location.
    fn expand_xspf(
        &mut self,
        source_path: &Path,
        document_bytes: &[u8],
        depth: usize,
    ) -> DocumentAttempt {
        let playlist = match parse_xspf_document(XspfParseRequest::new(
            document_bytes,
            PlaylistDocumentSource::local(source_path),
            self.xspf_limits,
        )) {
            Ok(playlist) => playlist,
            Err(_) => {
                self.record_issue(LocalPlaylistExpansionIssueKind::XspfParseFailed, depth);
                return DocumentAttempt::Unexpanded;
            }
        };

        DocumentAttempt::Expanded(self.expand_xspf_playlist(source_path, playlist, depth))
    }

    /// Сохраняет original XSPF groups только при complete source-track traversal.
    fn expand_xspf_playlist(
        &mut self,
        source_path: &Path,
        playlist: XspfPlaylist,
        depth: usize,
    ) -> ExpandedLocalPlaylistDocument {
        let mut entries = Vec::with_capacity(playlist.tracks().len());
        let mut source_complete = true;

        for track in playlist.tracks() {
            if self.summary.cancelled {
                source_complete = false;
                break;
            }
            let track = track.clone();
            let Some((include_path, include_format)) = xspf_include(&track) else {
                if !self.retain_leaf(depth) {
                    source_complete = false;
                    break;
                }
                entries.push(ExpandedLocalPlaylistEntry::XspfTrack(track));
                continue;
            };

            match self.expand_document(&include_path, include_format, depth.saturating_add(1)) {
                DocumentAttempt::Expanded(document) => {
                    entries.push(ExpandedLocalPlaylistEntry::IncludedDocument(Box::new(
                        document,
                    )));
                }
                DocumentAttempt::Unexpanded => {
                    entries.push(ExpandedLocalPlaylistEntry::UnexpandedInclude(
                        UnexpandedLocalPlaylistInclude::XspfTrack(track),
                    ));
                    if self.summary.cancelled {
                        source_complete = false;
                        break;
                    }
                }
            }
        }

        let xspf_groups = if source_complete {
            playlist.groups().to_vec()
        } else {
            Vec::new()
        };
        ExpandedLocalPlaylistDocument::new(
            source_path.to_path_buf(),
            LocalPlaylistDocumentFormat::Xspf,
            entries,
            xspf_groups,
            source_complete,
        )
    }

    /// Резервирует один aggregate leaf slot или публикует truncation один раз.
    fn retain_leaf(&mut self, depth: usize) -> bool {
        if self.summary.retained_items >= self.limits.maximum_items() {
            self.summary.item_truncations += 1;
            self.record_issue(LocalPlaylistExpansionIssueKind::ItemBudgetExceeded, depth);
            return false;
        }
        self.summary.retained_items += 1;
        true
    }

    /// Добавляет detail только в пределах cap, сохраняя lossless total.
    fn record_issue(&mut self, kind: LocalPlaylistExpansionIssueKind, depth: usize) {
        self.summary.total_diagnostics = self.summary.total_diagnostics.saturating_add(1);
        if self.issues.len() < self.limits.maximum_diagnostics() {
            self.issues
                .push(LocalPlaylistExpansionIssue::new(kind, depth));
        } else {
            self.summary.omitted_diagnostics += 1;
        }
    }

    /// Поднимает parser-owned omitted count без попытки восстановить details.
    fn record_pre_omitted_diagnostics(&mut self, omitted_count: usize) {
        self.summary.total_diagnostics =
            self.summary.total_diagnostics.saturating_add(omitted_count);
        self.summary.omitted_diagnostics = self
            .summary
            .omitted_diagnostics
            .saturating_add(omitted_count);
    }
}

/// Определяет include только по exact native local locator и supported extension.
fn m3u_include(
    import_draft: &PlaylistSingleImportDraft,
) -> Option<(&Path, LocalPlaylistDocumentFormat)> {
    let local_locator = import_draft.reopen_locator().expose_local_for_reopen()?;
    let native_path = local_locator.expose_native_path_for_open()?;
    let format = document_format(native_path)?;
    Some((native_path, format))
}

/// XSPF alternatives не выбираются: recurse допустим только для одного local candidate.
fn xspf_include(track: &XspfTrack) -> Option<(PathBuf, LocalPlaylistDocumentFormat)> {
    let [candidate] = track.location_candidates() else {
        return None;
    };
    let parsed_uri = Url::parse(candidate.expose_uri_for_admission()).ok()?;
    if parsed_uri.scheme() != "file"
        || parsed_uri.query().is_some()
        || parsed_uri.fragment().is_some()
    {
        return None;
    }
    let native_path = parsed_uri.to_file_path().ok()?;
    let format = document_format(&native_path)?;
    Some((native_path, format))
}

/// Extension sniff не выполняет lossy conversion и не читает content.
fn document_format(path: &Path) -> Option<LocalPlaylistDocumentFormat> {
    let extension = path.extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("m3u") {
        return Some(LocalPlaylistDocumentFormat::M3u);
    }
    if extension.eq_ignore_ascii_case("m3u8") {
        return Some(LocalPlaylistDocumentFormat::M3u8);
    }
    if extension.eq_ignore_ascii_case("xspf") {
        return Some(LocalPlaylistDocumentFormat::Xspf);
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Fake reader моделирует cancellation строго после complete root read.
    struct CancellingReader<'token> {
        /// In-memory exact document bytes.
        documents: HashMap<PathBuf, Vec<u8>>,
        /// Token текущего request-а.
        cancellation: &'token LocalPlaylistExpansionCancellation,
        /// Первый read отменяет последующие documents.
        cancel_after_first_read: bool,
        /// Exact read count.
        reads: usize,
    }

    impl LocalDocumentReader for CancellingReader<'_> {
        fn canonicalize(&mut self, path: &Path) -> io::Result<PathBuf> {
            Ok(path.to_path_buf())
        }

        fn read_bounded(
            &mut self,
            path: &Path,
            maximum_bytes: usize,
        ) -> io::Result<BoundedDocumentRead> {
            let bytes = self.documents.get(path).cloned().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "fake document отсутствует")
            })?;
            self.reads += 1;
            if self.cancel_after_first_read && self.reads == 1 {
                self.cancellation.cancel();
            }
            if bytes.len() > maximum_bytes {
                return Ok(BoundedDocumentRead::LimitExceeded);
            }
            Ok(BoundedDocumentRead::Complete(bytes))
        }
    }

    #[test]
    fn cancellation_is_observed_between_documents() {
        let cancellation = LocalPlaylistExpansionCancellation::new();
        let root = PathBuf::from("/lists/root.m3u");
        let child = PathBuf::from("/lists/child.m3u");
        let mut reader = CancellingReader {
            documents: HashMap::from([
                (root.clone(), b"child.m3u\n".to_vec()),
                (child, b"song.mp3\n".to_vec()),
            ]),
            cancellation: &cancellation,
            cancel_after_first_read: true,
            reads: 0,
        };
        let request = LocalPlaylistExpansionRequest::new(
            &root,
            LocalPlaylistExpansionLimits::default(),
            M3uParserLimits::default(),
            XspfParserLimits::default(),
            &cancellation,
        );

        let result = expand_with_reader(request, LocalPlaylistDocumentFormat::M3u, &mut reader)
            .expect("request valid");

        assert!(result.summary().cancelled());
        assert_eq!(reader.reads, 1);
        assert!(matches!(
            result
                .root_document()
                .expect("root parsed")
                .entries()
                .first(),
            Some(ExpandedLocalPlaylistEntry::UnexpandedInclude(_))
        ));
    }

    #[test]
    fn stale_token_cannot_cancel_new_request_token() {
        let stale = LocalPlaylistExpansionCancellation::new();
        stale.cancel();
        let current = LocalPlaylistExpansionCancellation::new();

        assert!(stale.is_cancelled());
        assert!(!current.is_cancelled());
    }
}
