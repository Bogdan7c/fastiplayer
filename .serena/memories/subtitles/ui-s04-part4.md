# Subtitles S04 part 4: drop of subtitle files
- classify: `ClassifiedItem::SubtitleFile` (by `subtitle_sidecar::is_subtitle_file_extension`, before MediaFile). Folder walk is media-only (whitelist), so .srt in folders never reach the queue.
- route: `OpenStep::AttachSubtitlesToCurrentMedia` (lone subs, any target), `QueueSubtitlesForOpenedMedia` (video/folder/url + subs on Video target; inserted FIRST), playlist-target mixed -> `DropNotice::SubtitlesNeedPlayingVideo` toast, nothing attached.
- host: `ExternalOpenHost::{attach_subtitle_files_to_current_media, queue_subtitle_files_for_opened_media}` -> `AppState::{attach_dropped_subtitle_files, queue_dropped_subtitle_files}` (state/subtitles/app_state_api.rs).
- Lifetime of queued files: `state/dropped_subtitles.rs` (`OpenActivity`, `AppState::settle_dropped_subtitles`) called at the end of `poll_local_file_open_job`; when no open work is pending the queue is cleared. No single failure hook exists (prep/confirm-cancel/strong-open/walk fail separately).
- Origin label for ChosenByUser is "added" (menu + toast).
- E2E tests: state/subtitles/tests/drop_flow.rs (real dispatcher + real SubtitleOwner; AppState can't be built without a window).
