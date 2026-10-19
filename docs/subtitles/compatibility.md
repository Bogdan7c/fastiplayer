# Fastiplayer: subtitle compatibility matrix

**Version 0.2.0** (not released yet: in `main` after `v0.1.0-alpha.1`). This matrix closes [issue #1](https://github.com/Bogdan7c/fastiplayer/issues/1) and the public plan [`user/subtitles/`](../../user/subtitles/README.md) (sessions 01–15, final acceptance 19 October 2026). Later changes update this file and name the version they ship in.

Legend:
- **Supported** — works end to end in the player. It is backed by automated tests: pixels read back after the GPU pass where the feature is visible, or parser and model tests for timing and markup. Many rows also have a dated manual acceptance in `user/subtitles/results/NN.md`.
- **Partial** — works with the gaps listed in its section.
- **Not supported** — known and refused safely: the track is listed greyed out with a reason, or the file is rejected with a message. Video keeps playing.
- **Backlog** — not planned yet.
- **Full** in the parser sections — every feature of that row is parsed into the model.

This matrix does not promise every historical subtitle format. A format or feature that is not listed here is not supported.

User guide (CC button, menus, hotkeys, edit mode, files next to the video): [user-guide.md](user-guide.md).

## Summary: formats and sources

| Format | File next to the video / dropped / "Load file…" | Inside MKV | Inside MP4 | YouTube (yt-dlp) | HLS | DASH (native) |
|---|---|---|---|---|---|---|
| SubRip (SRT) | Supported (`.srt`) | Supported | — | — | — | — |
| WebVTT | Partial (`.vtt`, see "WebVTT") | Supported (text and markup only, no positioning) | — | Supported (fallback when srv3 is missing) | Supported (VOD and live) | Supported (sidecar file and fMP4 `wvtt`) |
| ASS / SSA | Supported (`.ass`, `.ssa`) | Supported, with attached fonts | — | — | — | — |
| mov_text (`tx3g`) | — | — | Supported | — | — | — |
| PGS (Blu-ray pictures) | Not supported (`.sup`) | Supported | — | — | — | — |
| YouTube srv3 | Not searched for | — | — | Supported (manual, auto, auto-translated) | — | — |
| TTML / IMSC 1.1 Text | Not searched for | — | — | — | — | Supported (sidecar file and fMP4 `stpp`) |
| VobSub, DVB, other picture codecs | Not supported | Not supported (greyed out) | Not supported | — | — | — |
| MicroDVD, SAMI, SubViewer, LRC and other legacy text formats | Not supported | — | — | — | — | — |

Subtitle tracks inside MPEG-TS, M2TS and FLV files are not read (backlog). DASH manifests produced by yt-dlp carry no text sets, so a YouTube video shows its yt-dlp tracks instead.

## Summary: behaviour by kind of track

| Capability | Simple text (SRT, WebVTT, mov_text, srv3, TTML) | ASS / SSA | PGS |
|---|---|---|---|
| Shown at the time of the frame actually drawn; pause keeps it; seek and new video leave no stale cue | Supported | Supported | Supported |
| Track switch during playback | Supported (embedded: instant from the cache; files, web and stream tracks: loaded on selection) | Supported | Supported |
| Sync offset (`Z` / `X`, menu) and per-video memory of track and offset | Supported | Supported | Supported |
| Auto-selection by preferred languages; forced tracks shown with CC off | Supported | Supported | Supported (also forced pictures inside a normal track) |
| User style (colours, box / outline, size, position) | Supported | Scale and vertical lift only; "Force my style" replaces the author's look | Not applied (author pictures) |
| On the black bars below the picture | Allowed | Never | Never |
| Lift above the control panel | Supported | Main lines only (setting `ass_position_mode`) | Pictures in the lower half only |
| Broken input | Bad cues skipped and counted; a file with no cue is "unavailable" with a reason | Bad lines skipped; an oversized header falls back to plain text | Broken segments skipped and counted |

## How support is verified (issue #1 acceptance criteria)

**1. Supported fixtures are drawn at the right time, including seek and track changes.** GPU read-back tests in `crates/app-egui/src/state/subtitles/tests/` render the real overlay pass and read the frame back:
- files next to the video: `gpu_chain.rs` (`subtitle_file_next_to_the_video_reaches_the_bottom_of_the_picture_and_only_while_the_cue_lasts`, on/off toggling);
- embedded SRT, track switch, mov_text, attached fonts: `embedded_gpu.rs` (`switching_the_track_during_playback_changes_the_rendered_text_from_cache`, `mp4_mov_text_cue_reaches_the_gpu_frame`);
- ASS: `ass_layer1_gpu.rs`, `ass_animation_gpu.rs`, `ass_karaoke_gpu.rs`, `ass_layer3_gpu.rs`, `ass_position_lift_gpu.rs`;
- PGS with seek back from the cache: `embedded_pgs_gpu.rs`, `embedded_pgs_placement_gpu.rs`;
- YouTube download and word-by-word captions: `web_gpu.rs`, `word_reveal_gpu.rs`;
- HLS and DASH opened for real: `stream_hls_vertical.rs`, `stream_dash_gpu.rs`, `stream_gpu.rs`.

Seek and frame timing of text tracks are also checked at the overlay level: `presentation.rs::seek_past_the_cue_hides_it_on_the_very_first_frame_and_seeking_back_restores_it`. Every source was also checked live; the final pass is in `user/subtitles/results/15.md`.

**2. Styled and picture references have a reproducible visual check; broken input fails safely.**
- ASS and styled text are compared with **libass 0.17.5** reference PNGs: 187 cases and 327 images in `crates/subtitle-raster/tests/fixtures/libass/`, tests in `crates/subtitle-raster/src/libass_parity/`. Drawings and strokes must match exactly; events and text match within per-case measured tolerances, with at most 1 % of the area differing.
- The references are regenerated by `scripts/subtitle-reference/generate.py`. It needs a system libass, but neither the build nor CI does.
- PGS pictures are checked pixel by pixel on synthetic streams (`crates/subtitle-pgs/src/decoder/tests.rs`) and on the GPU (`embedded_pgs_gpu.rs`).
- Broken input: malformed SRT / WebVTT / ASS / srv3 / TTML / PGS, size and count limits, broken and oversized fonts and absurd ASS values give typed errors or skip-and-count, never a panic. Tests are in `crates/subtitle-formats/tests/`, `crates/subtitle-pgs/src/`, `crates/subtitle-raster/src/renderer/tests/{ass_robustness,limits,font_fallback}.rs` and `crates/subtitle-raster/tests/fonts_library.rs`.
- A subtitle failure never stops video. This is checked in `crates/player-core/src/session/tests/subtitle_feed.rs` (full queue, receiver gone) and in the app owner tests (`sidecar_job.rs`, `stream_tracks.rs`, `web_tracks.rs`). DASH has an extra test proving a text 404 never reaches video recovery: `crates/web-media-dash/src/text/isolation_tests.rs`.

**3. Published matrix and limits.** This file. Known limits are listed in each section and collected in "Known limitations (summary)" at the end. Subtitle crates stay independent of the GPU and UI: `scripts/check-refactor-guardrails.py` allows `subtitle-core`, `subtitle-formats`, `subtitle-pgs`, `subtitle-raster` and `subtitle-sidecar` only their listed dependencies (no wgpu, egui, winit or `render-*`).

Not covered by a dedicated automated test, so stated with care below:
- legacy encodings other than Windows-1251;
- Indic shaping against libass;
- the effect of TTML `seq` / `timeBase` counting;
- a device node dropped as subtitles (FIFO and directory are tested);
- moving the window between monitors with different scales (one-monitor rig).

## Formats

| Format | Source | Status | Session |
|---|---|---|---|
| SubRip (`.srt`) | sidecar file | Partial (see below) | 01 |
| WebVTT (`.vtt`) | sidecar file | Partial (see below) | 01 |
| SubRip / WebVTT inside MKV | embedded | Supported (see "Embedded tracks") | 06 |
| mov_text (`tx3g`) inside MP4 | embedded | Supported (bold / italic / underline / colour) | 06 |
| ASS / SSA inside MKV | embedded | Supported, all libass tags and effects (see "ASS / SSA (sessions 08–10)") | 06; 08; layer 2: 09; layer 3: 10 |
| ASS / SSA sidecar file | sidecar | Supported, all libass tags and effects (see "ASS / SSA (sessions 08–10)") | 08; layer 2: 09; layer 3: 10 |
| PGS inside MKV | embedded | Supported (see "PGS (session 11)") | 11 |
| YouTube srv3 (`<timedtext format="3">`) | web (yt-dlp) | Supported: manual, auto (speech recognition) and auto-translated tracks, downloaded on selection (see "YouTube srv3 (session 12)" and "YouTube tracks (session 12)") | 12 |
| YouTube WebVTT | web (yt-dlp) | Supported as fallback when srv3 is unavailable for a track | 12 |
| TTML / IMSC 1.1 Text Profile (`<tt xmlns="http://www.w3.org/ns/ttml">`) | streaming / sidecar data | Supported (see "TTML / IMSC (session 13)"); delivered from DASH (see "DASH text tracks (session 13)") | 13 |
| HLS WebVTT (`EXT-X-MEDIA TYPE=SUBTITLES`, `X-TIMESTAMP-MAP`) | streaming (native HLS, HLS via yt-dlp) | Supported, VOD and live (see "Stream subtitle tracks in the app (session 13, HLS WebVTT)") | 13 |
| DASH text tracks: sidecar TTML / WebVTT files, fMP4 `stpp` (TTML) and `wvtt` (WebVTT) | streaming (native DASH) | Supported, VOD and live (see "DASH text tracks (session 13)") | 13 |
| VobSub, DVB, FLV subtitles | — | Backlog (embedded VobSub / DVB tracks are listed greyed out) | — |

## Rendering (crate `subtitle-raster`, session 02)

Pure-Rust rasterizer, a port of libass 0.17.5 behaviour (ISC). Output: a list of 8-bit coverage masks with one colour each (like libass `ASS_Image`), plus RGBA pictures for PGS (session 11). Any renderer can draw them; the wgpu pass is described in "On screen".

| Feature | Status |
|---|---|
| Glyph rasterizer, border stroker, shadow (libass port) | Full — bit-exact with libass on vector drawings |
| Plain text with the default style (Latin, Cyrillic, two lines, smart wrap, BorderStyle 1/3/4) | Partial — near pixel-exact with libass (a few to ~100 pixels differ per 720p frame due to FreeType vs skrifa outline rounding) |
| Right-to-left text (Arabic, Hebrew), complex shaping (Indic) | Partial — `unicode-bidi` + `harfrust`. Arabic is compared with a libass reference and differs by ~1 px on some glyphs (autohinter advance rounding). Indic scripts are shaped by the same engine, but no libass reference compares them yet |
| Built-in fonts | Noto Sans Regular/Bold (Latin, Greek, Cyrillic) + Regular for Arabic, Hebrew, Devanagari, Bengali, Tamil, Telugu, Kannada, Malayalam, Gujarati, Gurmukhi, Thai, Lao, Georgian, Armenian, Ethiopic, Khmer, Myanmar, Sinhala, symbols (~3.3 MiB, OFL-1.1) |
| CJK, emoji | from system fonts only; missing glyphs are drawn as `.notdef` and counted. The system fallback face is ranked like fontconfig: slant, language, nearest weight. The regional CJK variant (JP/SC/TC/HK/KR) follows the track language, otherwise the system language |
| First CJK cue | The needed system faces are loaded in the background while the render thread is idle (session 14), so the first cue does not wait for a ~20 MB font file. The cost of that load (~12–14 ms) was measured with `crates/subtitle-raster/examples/measure_render_time.rs`; no test asserts the time |
| Fonts attached to MKV | Supported (session 06): prepared off the UI thread, installed per video, removed when the video changes |
| User styles | "YouTube-like" (white text on a rounded 75% dark box per line) and "outline + shadow" |
| WebVTT placement | `line`, `position`, `size`, `align`, `\anN` anchors; regions and vertical text are drawn horizontally |
| Underline / strikethrough, synthetic bold / italic | Full (session 08) — also for SRT / WebVTT: a face without a real italic / bold is slanted / emboldened like libass |
| Span background (WebVTT `background`, srv3 `bc`, TTML `backgroundColor`) | Not supported (backlog): parsed into the model, drawn without the per-span box; the user style box still applies |
| Line breaks in CJK text without spaces | Simple formats (SRT, WebVTT, srv3, TTML, mov_text, web) wrap between CJK characters with kinsoku rules (no line starts with closing punctuation, small kana or the long-vowel mark; no line ends with an opening bracket; combining marks stay attached). ASS keeps libass behaviour: breaks only after spaces, so a long CJK line without spaces stays one line |
| ASS layer-1 override tags | Supported (session 08, see below) |
| ASS layer-2 tags: rotations, shear, 3D, `\org`, `\move`, `\fad` / `\fade` | Supported (session 09, see below) |
| ASS layer-3 tags: drawings `\p \pbo`, clip `\clip \iclip` (rectangle and vector), blur `\blur \be` | Supported (session 10a, see below) |
| ASS `\t`, karaoke `\k \K \kf \ko \kt`, `Banner` / `Scroll up` / `Scroll down` effects | Supported (session 10b, see below) |

## On screen (session 03)

| Feature | Status |
|---|---|
| GPU pass between video and UI (menus and buttons always on top) | Full (wgpu); one draw per frame, texture upload only when the cue set, picture size or style changes |
| Timing | by the pts of the frame actually drawn; pause keeps the cue, seek drops it on the first new frame, cues of the previous file never appear |
| Placement | bottom of the picture area by default; position, the black bars and lifting above the controls: see "Appearance and position (session 05)" |
| Track choice | automatic by preferred languages, or from the CC menu (session 04, see below) |
| Files with a non-zero container start time (MPEG-TS) | supported for subtitle files beside the video and dropped files of a local MPEG-TS: they count from 0 (like ffmpeg and mpv), so the demuxer publishes the container start (earliest PTS of the first recording) and the file times are shifted by it. Embedded tracks (same timeline as the video), HLS/DASH stream tracks and web tracks are not shifted. The player position shown in the UI is still the raw container time; M2TS (192-byte) is not supported by the demuxer at all |

### Sidecar files next to a local video

Video `Movie.mkv`. Searched in the video folder, in `Subs`/`Sub`/`Subtitles`/`Subtitle` (any case) and in `Subs/Movie/`.

| File | Found | Language |
|---|---|---|
| `Movie.srt`, `movie.SRT` | yes (exact name is preferred) | — |
| `Movie.ru.srt`, `Movie.English.srt`, `Movie_en.srt`, `Movie [Forced].srt` | yes | from the suffix (`forced`, `sdh` are flags) |
| `Subs/Movie.en.srt`, `Subtitles/Movie.vtt` | yes | from the suffix |
| `Subs/Movie/2_English.srt` (any name inside a folder named after the video) | yes | from the name |
| `Other.srt`, `Movie2.srt`, `Extras/Movie.srt`, `Subs/Other/x.srt` | no | — |
| `.ass` / `.ssa` | yes, shown with the file's PlayRes, styles and all supported tags (sessions 08–10) | from the suffix |
| `.sup`, `.idx` / `.sub`, `.srv3`, `.ttml`, other extensions | not searched for | — |
| Non-UTF-8 file names | yes, paths are kept byte-exact | — |

## Controls (session 04)

| Feature | Status |
|---|---|
| CC button | right of Repeat, only when the video has at least one subtitle track; underlined when on. Window ≥ 498 pt: CC + ▾ menu; 470–497 pt: CC only (right-click opens the menu); < 470 pt: hidden, hotkeys still work |
| Menu | tracks (`Russian — forced (SRT, file)`; a file without a language shows its file name), Off, Load file…, Delay with Reset; unavailable tracks are greyed with the reason on hover |
| Hotkeys | `C` on/off, `V` next track (cycles through Off), `Z` / `X` delay −0.1 s / +0.1 s (one updating notification) |
| Auto-selection | off by default; when on: first track matching `subtitles.preferred_languages` (empty = system language; `rus` matches `ru`), otherwise the first track; forced tracks are shown even when subtitles are off |
| Remembered | whether subtitles are on (config `subtitles.enabled`); per-video track, Off and delay: see "Audio tracks and per-video memory (session 07)" |
| Drop a subtitle file on the window | attaches to the current video (local or web) and shows it; the queue is not changed. Nothing playing → "Open a video first, then add subtitles." Video + subtitle file dropped together → attached to that video; dropped on the playlist panel → not attached (notification) |
| Language from a dropped / loaded file name | `Movie.ru.srt`, `sintel_ru.srt`, `Movie English.srt`, `Movie [Forced].srt`; only known languages (no video name to compare with, so `The_Big_Lie.srt` is not "lie") |
| Settings | Settings → `subtitles`: show subtitles, preferred languages, style (box / outline, size, colors, box opacity), resource limits (config schema v23 since session 13; the full list of keys is in the [user guide](user-guide.md#settings)) |

## Appearance and position (session 05)

| Feature | Status |
|---|---|
| Edit on video | Settings → `subtitles` → "Edit appearance on video…" (needs an open video and no unapplied settings changes). Works paused and while playing; shows the real cue, or a sample when none is on screen |
| Mouse | drag the frame = position (any direction), drag a corner or wheel over the video = text size; double-click and wheel do not toggle fullscreen / change volume while editing |
| Panel | text color, box color, box opacity, Box / Outline, Reset, Cancel, Done; Esc = Cancel (an open color picker closes first) |
| Saving | Done, reopening Settings or quitting saves in one config transaction; Cancel / Esc restores the previous style exactly |
| Position | stored as fractions of the picture (`position_horizontal_percent`, `position_vertical_percent`), so it survives window resize and fullscreen; never leaves the picture sideways; simple formats may sit on the black bar under the picture |
| Above the controls | subtitles move up only when the control panel would cover them, following the panel's slide animation |
| ASS / SSA | `ass_force_user_style` and `ass_position_mode` apply (session 08, see "ASS / SSA (session 08)"). In `main_lines_avoid_controls` (default) and `author` modes, edit mode drags only the sample (shown where no cue is active) and plain-text cues; a real ASS cue is not moved by the drag |
| Known limit | the whole subtitle layer moves together, so a WebVTT cue placed by its author (e.g. at the top) moves with the user offset and with the lift above the controls |

## Text encodings (SRT, WebVTT files)

| Input | Result |
|---|---|
| UTF-8 with or without BOM | exact |
| UTF-16 LE/BE with BOM | exact |
| UTF-16 LE/BE without BOM | detected when the text contains Latin/ASCII (timings always do); pure non-ASCII UTF-16 without BOM is not detected |
| Legacy 8-bit (Windows-1251, KOI8-R, Windows-1250, Greek, CJK, …) | statistical guess (`chardetng`) with a Cyrillic preference hint; the result is recorded in the parse report (the UI does not show it yet). Windows-1251 and Windows-1256 (Arabic) are covered by tests or live checks; the other encodings rely on `chardetng` without dedicated tests. Very short files can be guessed wrong (see "Known limitations (session 14)") |

## SubRip (`.srt`) — Partial

Supported:
- optional cue numbers, CRLF/CR/LF, repeated blank lines;
- tolerant timings: `,` or `.` separator, extra spaces, missing hours (`MM:SS,mmm`), 1–3 fraction digits;
- tags `<i> <b> <u> <s>` (any case, nesting, unclosed/stray tags tolerated), `<font color>` (hex `#rgb`/`#rrggbb`, 16 basic names), `<br>`, `\N`;
- `{\an1}`…`{\an9}` screen anchor; other `{\…}` override blocks are stripped;
- broken cues are skipped and counted, the rest of the file still loads; empty cues are counted and skipped.

Gaps / differences:
- a fraction like `,5` is read as 0.5 s (decimal fraction); ffmpeg/VLC read it as 5 ms;
- HTML entities (`&amp;`) are not decoded in SRT;
- `<font face/size>` ignored; unknown `<font color>` values leave the text uncoloured;
- a timing line must be in the first two lines of a block.

## WebVTT (`.vtt`) — Partial

Supported:
- `WEBVTT` signature and header, `NOTE`, `STYLE` and `REGION` blocks (STYLE/REGION only before the first cue, as in the W3C spec);
- `X-TIMESTAMP-MAP` header (HLS segments, RFC 8216): parsed into a typed value for the stream loader; a malformed header is reported and ignored, cues still load;
- strict timestamps `[hh:]mm:ss.ttt`;
- cue settings `vertical`, `line` (number or %, with alignment), `position` (with alignment), `size`, `align`, `region`; invalid settings are ignored with a report entry, the cue stays;
- tags `<c> <i> <b> <u> <v> <lang> <ruby>/<rt>` with classes on any tag, entities (`&amp; &lt; &gt; &nbsp; &lrm; &rlm;`, numeric);
- karaoke timestamp tags (`<00:00:01.500>`) → per-span reveal time;
- STYLE subset: selectors `::cue`, `::cue(.class)`, `::cue(v[voice="X"])`; properties `color`, `background`/`background-color` (colour values), `font-weight`, `font-style`, `text-decoration(-line)` (underline, line-through); colours `#rgb(a)`, `#rrggbb(aa)`, `rgb()/rgba()`, 16 basic names, `transparent`. Author styling is kept on top of the user style (owner decision).

Gaps:
- default colour classes (`.white`, `.lime`, `.bg_*`, …) are not applied;
- other selectors (compound `.a.b`, `::cue(b)`, `::cue-region`, attribute/pseudo selectors) and at-rules are ignored and counted;
- other CSS properties (font size/family, shadows, outline, opacity, `var()`, `calc()`), `hsl()` and extended colour names are ignored;
- region/cue conflict rules and region scrolling are left to the renderer (session 03+);
- ruby annotation attaches to the first span of a multi-span base;
- only an empty line separates blocks (whitespace-only lines do not), as in the W3C spec.

## Embedded tracks (session 06)

Subtitle tracks inside the container (MKV, MP4) appear in the CC menu next to files found beside the video, with the word "embedded" in the label (language, title, forced / SDH flags and the format come from the container).

Supported:
- text codecs: SubRip (`S_TEXT/UTF8`), WebVTT, ASS / SSA, `mov_text`;
- every subtitle track of the video is read in the background, not only the selected one, so switching tracks during playback is instant and shows cues that were already read;
- seeking back shows previously read cues from the cache (duplicate packets are ignored);
- auto-select: preferred language, then non-SDH, then a file beside the video before an embedded track, then the container "default" flag, then list order; without a language match: a file beside the video, then the embedded default, then the first track. Forced tracks follow the same rules as before;
- fonts attached to MKV (`font/*`, `application/x-truetype-font`, `application/vnd.ms-opentype`, `application/font-sfnt`, `.ttf/.otf/.ttc/.otc`) are used for the video's subtitles; attachments over the limits (`max_attached_font_mb`, `max_attached_fonts_total_mb`) are not loaded and are reported in the log as counts only.

Limitations:
- picture tracks other than PGS (VobSub, DVB, BMP) and unknown codecs are listed greyed out ("not supported yet");
- a cue without a duration in the container is shown for `embedded_cue_max_open_duration_ms` (default 5 s) instead of "until the next cue", because the next cue has not been read yet;
- WebVTT inside MKV carries no positioning in the packet (only the text and markup), so cues are placed by the user style;
- ASS / SSA tracks are drawn with their own style header and layer-1 tags (session 08); a track that appears later through `TracksChanged` has no header and is drawn as plain text (tags stripped);
- like mpv, cues are known only after the demuxer has read them: after a forward seek, a cue that started before the target and is still active appears once the demuxer reaches its packet (the container is read from the nearest keyframe before the target, so this is usually immediate); a cue that was never read is not shown;
- if the packet queue between the player and the UI overflows, packets are dropped (counted, one warning in the log) while video and audio keep playing.

## PGS (session 11)

Blu-ray picture subtitles (`S_HDMV/PGS`) inside MKV are a normal, selectable embedded track, labelled "PGS, embedded", and take part in auto-select like any other track.

Supported:
- the pictures are drawn at the position and size the author set; the author's frame (usually 1920x1080) is fitted into the video picture keeping its proportions (one scale for both axes, centred), so letters are never squashed when the bars were cropped from the video; colours follow the stream palette (BT.601 for author frames up to 576 lines, BT.709 above) with its transparency;
- pictures in the lower half of the author frame (dialogue) rise above the control panel when it appears and follow the user's vertical offset, but never leave the video picture (never on the black bars); pictures higher up (signs) stay exactly where the author put them; the user text size, colours and horizontal offset are not applied to pictures;
- a picture without a duration in the container stays until the next display set (a "clear" set ends it); the end is applied in place, so a picture on screen never blinks when its end becomes known;
- forced pictures, "like a Blu-ray player": when subtitles are off and the video has no forced track, the PGS track of the preferred language shows only the pictures marked forced by the author (translations of foreign speech and signs); the CC button stays off. A container-forced track still takes precedence;
- seeking back shows pictures from the cache; every track of the video is read in the background, so switching tracks is instant;
- memory: the track being shown keeps its pictures within `pgs_displayed_track_cache_mb` (default 32 MB; the farthest from the playback position are dropped first, the picture on screen never); every other PGS track keeps only the last `pgs_background_track_window_s` seconds (default 60) before the playback position. Both settings take effect with the next opened video. Pictures are stored compressed (as in the stream) and decoded only when shown.

Limitations:
- after a seek the picture that was open (shown "until the next set") is dropped, because it is unknown whether it ended in the skipped part; a seek forward into the middle of a picture shows it from the next display set (pictures that were already closed stay in the cache);
- a broken segment is skipped and counted (the track keeps working); a display set that refers to a missing object or palette clears the screen instead of leaving an old picture; a picture larger than the memory budget is not shown;
- PGS inside MPEG-TS/M2TS, `.sup` files beside the video, VobSub and DVB subtitles are not supported (backlog).

## Audio tracks and per-video memory (session 07)

| Feature | Status |
|---|---|
| Audio button | `♪ ▾` left of CC, only when the video has at least two audio tracks; click opens the track list (`Russian — Dub (FLAC)`, `Track 2 (Opus)` without tags). Hidden before CC in narrow windows (shown from about 586 pt, with ▾ from about 614 pt); `B` still works |
| Hotkey | `B` next audio track (cycles), one updating notification `Audio: …` |
| Switching | the audio path is rebuilt for the new track (its own codec, rate and channels) and the player does a short seek to the current position, so audio and video stay in sync; Paused stays paused. Non-seekable sources continue from the next packets without a seek |
| Per-video memory | the chosen audio track, subtitle track (embedded, file beside the video, loaded / dropped file), Off and the subtitle delay are remembered per video (local files and links) and restored on reopen, after restart and when the queue reaches the video again. Priority: this video's memory, then auto-selection, then container flags |
| Storage | `track-memory.json` next to the config (schema v2 since session 12: web subtitle choice and YouTube audio language; v1 files still load); videos are keyed by a fingerprint of the path / link, a chosen subtitle file is stored by its path. Limit `playlist.track_memory_max_entries` (default 1000, 10–10000); the least recently used videos are forgotten first. A broken file is set aside as `track-memory.corrupt-*.json`, playback starts with an empty memory (warning in the log) |
| Not remembered | automatic choices; quality / candidate switches keep the current tracks; HLS/DASH audio version choice |
| Known limits | a track is found again by language + title + codec (+ position among tracks); a track in another language is never picked instead. The same web video opened by a different kind of link (manifest URL vs page URL) has separate memory |


## ASS / SSA (sessions 08–10)

Embedded (MKV `S_TEXT/ASS`, `S_TEXT/SSA`) and sidecar `.ass` / `.ssa` files are parsed into the model and drawn by the libass-parity rasterizer. Both V4+ (ASS) and V4 (SSA) style sections are read.

Always bound to the video picture: ASS is scaled from the script's PlayRes to the picture and is never drawn on the black bars.

Script header: `PlayResX/Y`, `LayoutResX/Y`, `ScaledBorderAndShadow`, `WrapStyle`, `Kerning`. Dialogue: layers (`Layer`), automatic collision handling between simultaneous lines, synthetic bold / italic, underline and strike-out.

The tag list was checked against `ass_parse_tags` of libass 0.17.5 (session 10b): every tag below is parsed in the same order and with the same argument rules, and every one of them changes the picture, the timing or the layout exactly where libass does.

| Group | Tags | Status |
|---|---|---|
| Font and style | `\b \i \u \s \fn \fs \fscx \fscy \fsc \fsp \fe` | Supported (session 08); `\fe` selects the paragraph direction only (`-1` = automatic, anything else = left to right), like libass |
| Colour and transparency | `\c \1c`–`\4c`, `\alpha \1a`–`\4a` | Supported (session 08) |
| Border and shadow | `\bord \xbord \ybord \shad \xshad \yshad` | Supported (session 08) |
| Alignment and position | `\an \a \pos` | Supported (session 08) |
| Reset and wrap | `\r \q \N \n \h` | Supported (session 08) |
| Rotation, shear, 3D | `\frx \fry \frz \fr \org \fax \fay` (libass perspective projection; a glyph behind the camera is dropped) | Supported (session 09) |
| Animation | `\move` (4 and 6 arguments), `\fad`, `\fade` (2 and 7 arguments) | Supported (session 09) |
| Transform animation | `\t` (all animatable tags, acceleration, nesting, rectangular `\clip` animation) | Supported (session 10b) |
| Clipping | `\clip \iclip` (rectangle and vector drawing; both may act at once) | Supported (session 10a) |
| Blur | `\blur \be` | Supported (session 10a) |
| Drawings | `\p \pbo` (move, line, cubic and b-spline commands) | Supported (session 10a) |
| Karaoke | `\k \K \kf \ko \kt` | Supported (session 10b) |
| Effects | `Banner`, `Scroll up`, `Scroll down` (`Effect` field) | Supported (session 10b) |

Unknown tags (not in the libass list) are skipped without hiding the rest of the text; the track keeps per-tag counts for diagnostics. The `Effect` field is parsed like libass: a prefix match with case (`Banner;`, `Scroll up;`, `Scroll down;`), numbers read like `atoi` after every `;` (at most four); too few numbers or an unknown name mean no effect and an ordinary event. The fade-away height (the last number) is ignored by libass and by us.

### Animation and the render thread (sessions 09–10)

Pictures are rasterized on a separate `subtitle-render` thread; the UI only picks a ready picture and never waits for it. A picture is valid for a time interval, so video frames are never held back by subtitles. A static scene is rasterized once. A scene with an animated event (`\move`, `\fad`, `\t`, `Banner` / `Scroll`) is re-rendered at most once per video frame and only while such an event is on screen; the thread prepares the next `prerender_frames` frames ahead. While paused, an animated frame costs nothing once its picture is ready. If the frame step is not known yet (the first frame), the picture is valid only for its own moment. After a long stall the observed step grows, so a picture can be reused for one frame that is slightly older than ideal.

What is redrawn when (session 10b): `\move`, `\fad`, a `\t` that changes the look of letters (size, colour, border, rotation, blur ...) and `Banner` / `Scroll` redraw the cue on every video frame while it is on screen (the scroll position is a fraction of a pixel, so the glyph raster changes with it). Karaoke and clips do not: the letters of a karaoke line are rasterized once and each frame only the fill border, the colour or the outline visibility changes; a `\t` that moves only a rectangular `\clip` and a `Scroll up/down` band re-cut the cached pictures. A `\kf` fill is cut again on every frame while it moves, while `\k` / `\ko` lines change only at syllable boundaries; a finished karaoke line, a not yet started one and a finished `\t` are static scenes and are not redrawn.

Banner and Scroll (libass `ass_apply_transition_effects`, `ass_render_event`): `Banner;delay;lefttoright` moves the line right to left (or left to right with a non-zero second number) by `time / delay` PlayRes pixels, wrap style 2 is forced, the lines of the block are aligned inside the widest line (`justify = alignment`, block aligned left); `Scroll up/down;y1;y2;delay` moves the block inside the band `y1..y2` (the picture is clipped to the band, rounded like a rectangular clip). The delay is rescaled like libass does it for VSFilter: `delay / scale` is truncated to an integer pixel of the layout resolution and is at least 1 (so a delay of 0 or a negative one means 1 ms per layout pixel). A `\pos` of the same event decides the axis the effect does not move. Such events never take part in collision handling and are "signs" (never lifted above the control panel).

Placement: events with `\pos`, `\move`, `\org`, rotation, shear or a Banner / Scroll effect are "signs": fixed in place and never lifted above the control panel. As in libass, only `\pos`, `\move`, `\org`, `\t` and the Banner / Scroll effects exclude an event from collision handling; a rotated or sheared line without them still avoids other lines.

### Settings

- `subtitles.ass_force_user_style` (default off): off — the author's colours, fonts, outline and shadow are drawn as written and `text_size_percent` scales the dialogue text, but not text placed with `\pos` (the author's explicit layout); on — the user's text colour, box colour / opacity and size replace the author's style, while `\pos` placement is kept.
- `subtitles.ass_position_mode`: `main_lines_avoid_controls` (default) — "main lines" (events without `\pos`/`\move`, aligned to the bottom: `\an1`-`\an3`) are lifted smoothly above the control panel while it is visible and return to the author's place when it hides; signs (`\pos`/`\move`, and since session 09 events with `\org`, rotation or shear) and top/middle-aligned lines never move; the user position offset is not applied to ASS; everything stays inside the picture. `author` — nothing moves (no lift, no user offset). `whole_layer` — the whole ASS layer moves like SRT (lift above the controls plus the user offset) but stays inside the picture on both axes. Edit-mode dragging does not move a real ASS cue in the first two modes.
- Limits (config schema v20): `max_ass_header_kb` (64–16384, default 1024), `max_ass_styles` (10–10000, default 1000), `max_ass_override_tags_per_event` (32–8192, default 512), `max_ass_events_on_screen` (16–2048, default 256: how many ASS events are drawn at once on one frame). SRT / WebVTT placement is unchanged (it may use the black bars).
- Pre-render (config schema v19): `prerender_frames` (0–64, default 4: how many frames ahead the subtitle render thread prepares pictures; 0 means the current frame only) and `prepared_overlays` (1–64, default 8: size of the queue of ready subtitle pictures). Migration v18 -> v19 only bumps the version; existing user values are kept.
- Blur memory (config schema v20): `blur_scratch_retained_mb` (1–1024, default 32: how much working memory of the blur `\blur` / `\be` the rasterizer keeps between frames; a blur that needs more still works, the extra memory is freed after the frame). The default equals the former built-in constant (two 1080p planes take about 8 MB, so 32 MB covers up to 4K without reallocating); the 1 MB floor avoids freeing after every frame, the 1024 MB ceiling protects against a typo. Takes effect with the next opened video. Migration v19 -> v20 only bumps the version; existing user values are kept.
- Glyph caches (config schema v21): `glyph_cache_mb` (8–1024, default 64: memory of the letter-mask cache and the merged-run cache, split two thirds to one third like libass), `glyph_outline_cache_items` (256–200000, default 10000: entries of the outline cache, counted in pieces, not bytes: it is not limited by the glyph cache memory) and `shaping_cache_mb` (1–256, default 4: memory of the shaped-text-run cache shared by all fonts; when full the least recently used runs are dropped, one by one). A letter moved by whole pixels (`\move`, Banner / Scroll) is not rasterized again; a run moved or recoloured without changing its shape is not merged or blurred again. Limits are applied at the start of every frame, so one frame may briefly exceed them. Takes effect with the next opened video. Migration v20 -> v21 only bumps the version; existing user values are kept.

### Known limitations

- A track added later through `TracksChanged` has no header (`codec_private`): it is drawn as plain text.
- Vertical text `\fn@Font` is not supported.
- Collision memory (stacking of simultaneous lines) is kept only for the cues active on the current frame, and it is reset at every new seek / scrub target (session 14), so the stacking at a target does not depend on where you jumped from. libass keeps that memory across any frames; during continuous playback both behave the same.
- Glyph storage size equals the display size (no separate `LayoutRes`-based storage scaling beyond the script header).
- A vector clip with coordinates outside the range libass accepts is invalid: like libass, it is not applied (the cue is drawn unclipped) and it is counted in the render statistics. A valid clip larger than the maximum canvas side still clips the visible part.
- "Force my style" (`ass_force_user_style` on) hides author drawings (`\p`) and ignores author blur (`\blur`, `\be`). This is an owner decision (17.10.2026): drawings and blur are part of the author's geometry and look, which the user style replaces. Letters and the rest of the cue are drawn as before.
- Drawings are drawn in a style with `Fontsize: 0` (the font size is irrelevant for them, as in libass). Letters in such a style are not drawn, while libass draws them at a size chosen by FreeType for a zero request (about unitsPerEm / 64 pixels); this difference is known and not ported.
- A vector clip mask is re-rasterized for every new animation key of the cue (the mask is part of the cached cue) and does not use the glyph caches. Letter outlines, borders, letter masks and merged run masks of animated scenes are cached across frames (see "Glyph caches" above), as in libass.
- Tag numbers that turn into NaN or infinity (for example `\frz1e999`) are normalized to 0 where libass relies on platform-dependent C conversions (angles, shadow, blur); the picture for such inputs may differ from libass on another platform than the reference one.
- Nesting of `\t` inside `\t` is limited to 16 levels (`MAX_TRANSFORM_NESTING`); libass has no limit, deeper tags are dropped and counted. Real files never go deeper than two levels.

## YouTube srv3 (session 12)

Parser: `crates/subtitle-formats/src/srv3/` (XML is read through `bounded-xml-reader` with budgets derived from `SubtitleParseLimits`). `sniff_text_subtitle_format` recognises `<timedtext` (with or without BOM / XML declaration). Sidecar `.srv3` files are not searched for yet (YouTube tracks arrive from the web).

| Feature | Status |
|---|---|
| Paragraphs `<p t d>` with entities, `<br/>`, literal newlines, U+200B padding trimmed | Full |
| Pens `<pen>`: `fc`/`fo` colour and opacity, `bc`/`bo` background (when `bo > 0`), `b`, `i`, `u` | Full (author colours override the user style, like other formats; span backgrounds are parsed but not drawn yet — see "Not done" in the rasterizer) |
| Pen outline / shadow (`et`, `ec`), size `sz` other than 100, font style `fs`, ruby | Ignored; each such pen is counted once in `ignored_style_rules` |
| Window position `<wp>` `ap` / `ah` / `av` | Mapped to `line` / `position` percentages and alignments; the standard bottom-centre (`ap=7 ah=50 av=100`, or no `<wp>`) is left to the user style |
| Window style `<ws>` `ju` (left / right / centre / justify -> centre) | Full; other `ws` attributes (`mh`, `pd`, `sd`) ignored and counted |
| Word timing `<s t>` | Each word has an absolute `reveal_at`; a word that has not appeared yet is not drawn and takes no space (the line grows as words appear); the picture is re-rasterized exactly at reveal moments |
| Rolling windows (auto captions, `<p w>`, `rc` rows) | The time axis of a window is cut at every paragraph start / end; each segment is one event with the lines of all active paragraphs, newest last, last `rc` kept (no `rc`: at most 16). `a="1"` line-separator paragraphs produce nothing |
| Missing `d` | Until the next paragraph start in the same window; for the last one 5 s |
| Duplicate paragraphs (same start, duration, window, text; YTSubConverter outline trick) | First kept, the rest counted as ignored style rules |
| `format` attribute | Absent is accepted; anything other than `3` (srv1 / srv2) is rejected as not srv3 |
| Broken XML, DOCTYPE, custom entities, XML budget exceeded | Fatal typed errors (`MalformedSrv3`, `NotSrv3`, `Srv3TooComplex`); the message never echoes file content |
| Per-paragraph problems (bad `t`, `d <= 0`, no text) | Skipped and counted in the parse report; the report "line number" is the paragraph ordinal |

Word reveal (`TextSpan::reveal_at`) also works for WebVTT karaoke timestamps (`<00:00:01.500>`). ASS karaoke is a separate path and is unchanged.

## TTML / IMSC (session 13)

Parser: `crates/subtitle-formats/src/ttml/` (XML through `bounded-xml-reader`, budgets derived from `SubtitleParseLimits`, like srv3). `sniff_text_subtitle_format` recognises a root `tt` that declares the namespace `http://www.w3.org/ns/ttml` (with or without prefix, BOM, XML declaration). Input must be UTF-8 or UTF-16 with BOM (decoded before XML reading); the primary source is W3C TTML2 / IMSC 1.1 Text Profile, `imscjs` was used as a behaviour reference only.

| Feature | Status |
|---|---|
| Clock time `hh:mm:ss[.fraction]`, `hh:mm:ss:ff[.subframe]`; offset time `h m s ms f t` | Full. `ttp:frameRate`, `ttp:frameRateMultiplier`, `ttp:tickRate` (default tick rate: frame rate when given, else 1); `ttp:timeBase` other than `media` is read as `media` and counted in `ignored_style_rules` |
| `begin` / `end` / `dur` on `body`, `div`, `p`, `span`, `br` | Parallel (`par`) semantics: child times are relative to the parent begin, the earlier of `end` and `begin + dur` wins, a missing end is the parent end, the result is clipped to the parent window; `timeContainer="seq"` is read as `par` and counted |
| Paragraph without any end in its ancestors | Shown until the next paragraph begins (so open cues never cover later ones); the last one until the latest time mentioned in the document; if there is none later than its begin, 5 s (structural fallback, like srv3). A TTML document taken from an fMP4 `stpp` sample is clipped to its sample interval and an open paragraph ends with the sample (ISO/IEC 14496-30). Times up to the `i64` microsecond range are accepted (live packagers write epoch-scale media times) |
| Spans / `br` with their own timing inside a `p` | The paragraph is cut at every change point (a simplified ISD): one event per interval with the text visible on it; equal adjacent intervals are merged |
| Styles: `<style xml:id>` with chained `style` references, inline `tts:*`, inheritance region -> body -> div -> p -> span | `tts:color`, `tts:backgroundColor` (on `p` / `span` only; a transparent value clears an inherited one), `fontStyle` (`oblique` = italic), `fontWeight` (`normal` / `bold`), `textDecoration` (underline / lineThrough and their `no*` forms), `textAlign`, `displayAlign`, `writingMode`. `tts:direction` is accepted (bidi is decided by the rasterizer). Reference cycles and chains deeper than 16 are cut |
| Colours | `#rrggbb`, `#rrggbbaa`, `rgb()`, `rgba()` (alpha 0..255), TTML named colours incl. `transparent`, `cyan`, `magenta` |
| Author colours | Kept as `SpanStyle` values; the user style still fills the fields the document leaves unset. Span backgrounds are parsed but not drawn yet (rasterizer "Not done") |
| Other `tts:*` properties (fontFamily, fontSize, lineHeight, textOutline, textShadow, opacity, padding, ...) and animation (`<set>`, `<animate>`) | Ignored; each ignored attribute / construct is counted in `ignored_style_rules` |
| Regions `<layout><region xml:id tts:origin tts:extent>` | `%` always; `px` when the root has `tts:extent="Wpx Hpx"` (otherwise counted and the region becomes the full frame). A region reference on `body` / `div` / `p` is inherited; `region` on `span` / `br` is ignored and counted; an unknown region id gives the default placement and is counted |
| Region -> `CuePlacement` | `size` = region width; `textAlign` start / left -> `position` = region left, `LineLeft`; centre -> region middle, `Center`; end / right -> region right, `LineRight`. `displayAlign` before -> `line` = region top, `Start`; after -> `line` = region bottom, `End`; centre -> `line` = region middle, `Center` (the rasterizer treats `Center` as `Start`, so a centre-aligned cue starts at the middle of the region). Without a region: default bottom-centre placement. The rasterizer multiplies `size` by the style `max_line_width`, so a block can be narrower than its region but never leaves it |
| `textAlign` not set | Centre (TTML says `start`; centring is the usual player behaviour) |
| Content not tied to a region while the document defines regions | Shown with the default placement (TTML would hide it) |
| Text | `<br/>` is a line break. `xml:space="default"`: runs of whitespace collapse to one space, spaces at line starts / line ends are dropped (a space between two spans is kept). `xml:space="preserve"`: spaces kept, `\n` is a line break. Leading / trailing line breaks are trimmed |
| `xml:lang` | The nearest `xml:lang` goes to `TextSpan::language` |
| Broken XML, DOCTYPE, custom entities | Fatal `MalformedTtml`; root other than a TTML `tt` -> `NotTtml`; XML budgets or more than 2048 text pieces in one paragraph -> `TtmlTooComplex`. Messages never echo file content |
| Per-element problems (bad time on `div` / `p` / `span`, empty text) | The element (with its subtree) is skipped; a bad time is recorded as an `InvalidTimestamp` issue, an empty paragraph as an empty cue; the report "line number" is the paragraph ordinal |

Not supported (this session): TTML1 TTAF namespace `http://www.w3.org/2006/04/ttaf1`, `smpte` / `clock` time bases, `seq` time containers, animation, vertical text drawing (the direction is stored, the rasterizer draws horizontally), ruby, image subtitles (IMSC image profile), `tts:origin` inside `region` children (`<style>` nested in `<region>`), `ttm:` metadata.

## YouTube tracks (session 12)

| Feature | Status |
|---|---|
| Source | yt-dlp `--dump-single-json` fields `subtitles` (manual) and `automatic_captions`; nothing is written to disk by yt-dlp |
| Kinds | Manual → `English`; speech recognition (`kind=asr`, no `tlang`) → `English (auto)`; auto-translation (`tlang`) → `Russian (auto, translated)`. Auto-dub videos have recognition per dubbed language (`ru-orig` …) |
| Filtered out | `live_chat`, HLS playlist entries inside `automatic_captions` (`protocol=m3u8_native`), formats other than srv3 / vtt |
| Menu | Manual and recognised tracks in the CC list (preferred languages first); translations in the "Auto-translate" submenu (preferred first, then alphabetical, scrollable). Same-named languages are told apart (`Chinese (Taiwan)`, `Portuguese (pt-BR)`) |
| Download | Only the selected track, on a background thread, size limit `subtitles.max_file_size_mb`; srv3 first, WebVTT when srv3 is missing / empty / not XML. Signed links live ~7 h; an expired link triggers one silent yt-dlp re-extraction |
| Errors | Video keeps playing; notification ("YouTube is limiting subtitle requests. Try again in a minute." for HTTP 429, connection / unavailable texts otherwise); the track stays in the menu and a click retries. No automatic background retries. Auto-translation usually gets 429 from YouTube (also for yt-dlp itself) — a YouTube limit, not a player bug |
| Auto-selection | Manual tracks win over recognition for the same preferred language; translations are never chosen automatically; `V` cycles only the main list |
| Memory | The chosen web track (language + kind) is remembered per video |
| Privacy | Signed subtitle links never appear in the UI or logs |
| Audio language (dubs) | Videos with several audio languages show them in the ♪ menu (`English (original)` first, then alphabetical); `B` cycles; choosing reopens the same item at the same position; default is the original; the choice is remembered per video |

## Stream subtitle tracks in the app (session 13, HLS WebVTT)

HLS subtitle renditions (`#EXT-X-MEDIA:TYPE=SUBTITLES`) appear in the CC menu as `English (stream)`, `Russian — forced (stream)`; the rendition NAME is added in quotes only when it says more than the language name. A stream can carry both yt-dlp tracks and HLS renditions: both lists are shown.

| Feature | Status |
|---|---|
| Loading | Only the shown track is loaded: a background feed downloads the subtitle segments around the playback position (`[subtitles.stream]` `ahead_s` / `behind_s`); selecting, deselecting, switching the track, changing the video or turning subtitles off cancels the feed. Video never waits for subtitles |
| Auto-select | `DEFAULT=YES` acts like the container default flag; `AUTOSELECT=NO` renditions are never picked automatically (manual choice stays possible); `FORCED=YES` renditions of a preferred language are shown even with CC off, like embedded forced tracks |
| Timing | Cues are mapped to the player timeline (`X-TIMESTAMP-MAP`, same origin as the video). Without the header: VOD cues are taken relative to the start of their discontinuity epoch (hls.js behaviour); live cues follow RFC 8216 §3.5 (no header means `MPEGTS:0` = `LOCAL:0`, so the cue time is the container time of the video, 33-bit wrap aware) and have no exact segment anchor, so the live window stays at the live edge. The segment start times of a sliding live playlist stay stable between reloads (re-anchored by the overlapping media sequences); a gap between reloads re-anchors the timeline and drops the learned offset. Segments that left the feed window are forgotten, so seeking back after the app trimmed old cues fetches them again. The feed window is built around the cue lookup time (player position minus the user's sync offset), and changing the offset re-centres it |
| Failures | A skipped segment is only logged. A segment that never gets its video timeline origin (evicted from the pending list or gone from the playlist) counts as a failure too. After `failure_threshold` segments in a row the track is reported once ("Subtitles for this stream are unavailable right now."); it stays in the menu and selecting it again starts a new attempt. There is no automatic retry |
| Live | Cues older than `live_retention_s` behind the lookup position are dropped by rebuilding the track timeline when at least as many are dead as alive (memory stays below twice the history). VOD tracks are never trimmed |
| Remembered choice | The choice is remembered per video as language + rendition name + forced flag (`track-memory.json` schema 3); URLs are never stored |
| Limits | `[subtitles.stream]`: `ahead_s` 60, `behind_s` 30, `max_segment_kb` 1024, `max_pending_segments` 8, `failure_threshold` 3, `live_retention_s` 600 (must not be smaller than `behind_s`); taken from the next feed start |

## DASH text tracks (session 13)

Text `AdaptationSet`s of a native DASH MPD (`.mpd` opened without yt-dlp) appear in the CC menu like HLS renditions (`English (stream)`; the `Label` is added in quotes when it says more than the language name). Specs: DASH-IF IOP (text tracks), ISO/IEC 23009-1 (`Role`, `Label`), ISO/IEC 14496-30 (`stpp` / `wvtt` in ISOBMFF).

| Feature | Status |
|---|---|
| Detected sets | `contentType="text"`, or `mimeType` `application/ttml+xml` / `text/vtt`, or `application/mp4` with codecs `stpp*` / `wvtt` on the set. A set with no such evidence of its own is text when ALL its `Representation`s carry text evidence (`application/mp4` + `stpp`/`wvtt`, `text/vtt`, `application/ttml+xml`); a mixed set stays audio/video |
| Formats | `SidecarTtml` / `SidecarWebVtt` (one file per Period, times relative to the Period start), `SegmentedStpp` / `SegmentedWvtt` (fMP4: SegmentTemplate incl. `SegmentTimeline`, SegmentList, or a whole fragmented file via SegmentBase / single resource). TTML / IMSC cues keep their regions (`tts:origin` / `tts:extent` -> cue placement) |
| Timing | Same player timeline as the video. fMP4 text uses the track media timeline: `player = media time - presentationTimeOffset + Period start - session origin` (the very mapping of A/V packets). SegmentList has no PTO, so the first sample of the FIRST fetched segment of a Period (`tfdt`) is taken as that segment's start and later segments reuse the same offset (like A/V `NormalizeAtFirstPacket`); sparse tracks therefore keep their gaps. WebVTT hours accept up to 6 digits (covers epoch-scale `wvtt` times until ~2084). Live: the session origin is the one the live video uses |
| Track info | language (set / representation `lang`), name (`Label`, else "Captions" for role `caption`, "Forced" for `forced-subtitle`, else "Subtitles"; duplicates get " (2)"), `forced-subtitle` role = forced, role `main` = default (else the first regular track), always auto-selectable |
| Skipped sets | `ContentProtection`, unknown MIME/codec, broken addressing: the set is skipped and counted (`skipped_text_adaptation_set_count`); audio/video are never affected. A Period with only text sets still fails like before (no A/V) |
| Live | The text plan is rebuilt with every accepted MPD refresh; the track index is re-read at `minimumUpdatePeriod` (at least 1 s) |
| Failures | Same policy as HLS: a segment that fails after the built-in HTTP retries is skipped (logged without the URL), repeated failures give one "Subtitles for this stream are unavailable right now." notification |
| Not supported | `wvtt` `ctim` (continuation of a split cue), non-fragmented MP4 text, text sets with `ContentProtection`, plain-text segments with PTO (segmented sidecar TTML / WebVTT is read Period-relative, a known limitation), yt-dlp DASH manifests (they carry no text sets), re-addressing of a failed live text segment after a CDN token rotation (the segment is skipped, the next index refresh uses the new address) |

## Edge cases (session 14)

| Case | Behaviour |
|---|---|
| NUL bytes in SRT / WebVTT | Removed after decoding (editors that pad files with zeros no longer swallow the last cue). ASS stops at the first NUL like libass; the cut is reported in the parse report |
| Huge single cue (megabytes of `<`, `{`, unclosed `<c>`) | Parsed in linear time |
| Thousands of cues with the same start | Loaded in O(n log n) (duplicates still ignored) |
| File content vs extension | SRT / WebVTT / ASS are recognised by content; the extension is the fallback (an ASS file named `.srt` works) |
| A file without any cue | The track is unavailable with "No subtitles could be read from this file." |
| A dropped / "Load file…" file that fails | One notification with the file name (no folder) and the reason: cannot be opened, too large, or nothing could be read. Files found automatically next to the video only show the grey hint in the CC menu |
| FIFO, directory or another non-regular file dropped as subtitles | Rejected before reading with "… is not a regular file, so it can't be used as subtitles."; loading and app exit never hang. A FIFO next to the video is not offered as a track. Tests cover FIFO and directory; a device node goes through the same file-type check |
| CJK text without spaces | Simple formats (SRT, WebVTT, srv3, TTML, mov_text, web) break lines between CJK characters with kinsoku rules; ASS keeps the libass behaviour (breaks only after spaces) |
| First CJK cue with a system font | The needed system font faces are prepared in the background while the render thread is idle, once per shown track |
| Network outage on HLS / DASH subtitles | Failed segments are retried with a growing pause within `network.reconnect_wait_ms`; only an outage longer than that makes the track unavailable (one notification) |
| MPEG-TS with a non-zero start (typical 1.4 s) | Subtitle files next to a local TS (and dropped files) are shifted by the container start, so they count from 0 as in ffmpeg / mpv. Embedded, stream and web tracks are not shifted; the user sync offset is applied on top and is not affected |
| Display scale change | The interface keeps the monitor scale (previously 150 % became 225 %); checked live by changing the KDE scale. Moving the window between monitors with different scales was not checked (one-monitor rig) |

### Known limitations (session 14)

- Color emoji are drawn as a monochrome outline (COLR / CBDT / sbix are not read).
- WebVTT `vertical` cues are drawn horizontally.
- Very short cp1251 files may be detected as cp1252; there is no manual encoding choice yet.
- Negative timestamps in SRT / WebVTT drop the cue instead of clamping it to 0.
- The player position and duration of MPEG-TS files are still the raw container time (1.4 s at the start); 192-byte M2TS is not opened.
- CJK line breaking has no dictionary segmentation; Thai and Khmer without spaces do not wrap.
- When a video and a subtitle file are dropped together in one gesture, a failing subtitle file only shows the grey hint in the menu (no notification).
- Simple subtitles of a vertical video stay within the picture width (they may use the top / bottom bars, not the side bars) — owner decision.

## Known limitations (summary)

The sections above give the details. This list collects the limits a viewer is most likely to notice in 0.2.0.

- **Formats:** VobSub, DVB, `.sup` files, subtitles inside MPEG-TS / M2TS / FLV, and legacy text formats (MicroDVD, SAMI, SubViewer) are not supported. TTML1 (TTAF namespace) and the IMSC image profile are not supported either.
- **Look:**
  - per-span backgrounds are not drawn;
  - colour emoji are drawn as a monochrome outline;
  - WebVTT `vertical`, TTML vertical writing and ASS `\fn@Font` are drawn horizontally;
  - ASS `Fontsize: 0` letters are not drawn (libass draws them tiny).
- **Text:**
  - encodings of very short legacy files can be guessed wrong, and there is no manual encoding choice yet;
  - negative SRT / WebVTT times drop the cue;
  - Thai and Khmer without spaces do not wrap.
- **Timing:**
  - an embedded cue without a duration is shown for `embedded_cue_max_open_duration_ms`;
  - after a forward seek, an embedded cue that started earlier appears once the demuxer reaches its packet (like mpv);
  - the player position of an MPEG-TS file is the raw container time.
- **Tracks:**
  - an ASS track that appears later through `TracksChanged` has no style header and is drawn as plain text;
  - the same web video opened through a different kind of link has separate per-video memory.
- **Streams:**
  - no automatic retry after a stream track is reported unavailable;
  - DASH `wvtt` `ctim`, non-fragmented MP4 text and protected text sets are not supported.
- **Resources:** the GPU subtitle atlas only grows within one video; the cue cache and the ASS layer cache count shared memory twice, so `render_cache_mb` evicts earlier than it could.

Bugs and wishes: [issue tracker](https://github.com/Bogdan7c/fastiplayer/issues) with the `area/subtitles` label.
