# Fastiplayer: subtitle compatibility matrix (draft)

Status: **draft**, updated by each session of the subtitles plan (`user/subtitles/`). Final version — session 15.

Legend: **Full** — format features are parsed into the model; **Partial** — parsed with the listed gaps; **Planned** — scheduled in a later session; **Backlog** — not planned yet.

Session 01 delivers parsing only (crates `subtitle-core`, `subtitle-formats`). Nothing is shown on screen until session 03.

## Formats

| Format | Source | Status | Session |
|---|---|---|---|
| SubRip (`.srt`) | sidecar file | Partial (see below) | 01 |
| WebVTT (`.vtt`) | sidecar file | Partial (see below) | 01 |
| SubRip / WebVTT inside MKV | embedded | Supported (see "Embedded tracks") | 06 |
| mov_text (`tx3g`) inside MP4 | embedded | Supported (bold / italic / underline / colour) | 06 |
| ASS / SSA text inside MKV | embedded | Text only, default style (no ASS styles or tags until the libass port) | 06; styles 08–10 |
| ASS / SSA sidecar file | sidecar | Planned (libass port) | 08–10 |
| PGS inside MKV | embedded | Listed greyed out ("not supported yet"); picture rendering planned | 11 |
| YouTube srv3 / WebVTT | web | Planned | 12 |
| HLS WebVTT, DASH TTML/IMSC | streaming | Planned | 13 |
| VobSub, DVB, FLV subtitles | — | Backlog (embedded VobSub / DVB tracks are listed greyed out) | — |

## Rendering (crate `subtitle-raster`, session 02)

Pure-Rust rasterizer, a port of libass 0.17.5 behaviour (ISC). Output: a list of 8-bit coverage masks with one colour each (like libass `ASS_Image`). Not on screen yet; the GPU pass is session 03.

| Feature | Status |
|---|---|
| Glyph rasterizer, border stroker, shadow (libass port) | Full — bit-exact with libass on vector drawings |
| Plain text with the default style (Latin, Cyrillic, two lines, smart wrap, BorderStyle 1/3/4) | Partial — near pixel-exact with libass (a few to ~100 pixels differ per 720p frame due to FreeType vs skrifa outline rounding) |
| Right-to-left text (Arabic, Hebrew), complex shaping (Indic) | Partial — `unicode-bidi` + `harfrust`; Arabic differs from libass by ~1 px on some glyphs (autohinter advance rounding) |
| Built-in fonts | Noto Sans Regular/Bold (Latin, Greek, Cyrillic) + Regular for Arabic, Hebrew, Devanagari, Bengali, Tamil, Telugu, Kannada, Malayalam, Gujarati, Gurmukhi, Thai, Lao, Georgian, Armenian, Ethiopic, Khmer, Myanmar, Sinhala, symbols (~3.3 MiB, OFL-1.1) |
| CJK, emoji | from system fonts only; missing glyphs are drawn as `.notdef` and counted. The system fallback face is ranked like fontconfig: slant, language, nearest weight. The regional CJK variant (JP/SC/TC/HK/KR) follows the track language, otherwise the system language |
| First CJK cue | ~12–14 ms once per run (loading a ~20 MB system font); background warm-up — session 14 |
| Fonts attached to MKV | Supported (session 06): prepared off the UI thread, installed per video, removed when the video changes |
| User styles | "YouTube-like" (white text on a rounded 75% dark box per line) and "outline + shadow" |
| WebVTT placement | `line`, `position`, `size`, `align`, `\anN` anchors; regions and vertical text are drawn horizontally |
| Underline/strikethrough, span background, synthetic bold/italic, `\N`-less CJK line breaking | not yet (later sessions / backlog) |
| ASS override tags, karaoke, blur, clip, rotations | Planned — sessions 08–10 |

## On screen (session 03)

| Feature | Status |
|---|---|
| GPU pass between video and UI (menus and buttons always on top) | Full (wgpu); one draw per frame, texture upload only when the cue set, picture size or style changes |
| Timing | by the pts of the frame actually drawn; pause keeps the cue, seek drops it on the first new frame, cues of the previous file never appear |
| Placement | bottom of the picture area (never on the letterbox bars yet — bars and lifting above the controls: session 05) |
| Track choice | automatic by preferred languages, or from the CC menu (session 04, see below) |
| Files with a non-zero container start time (some MPEG-TS/M2TS) | not yet: sidecar times are compared with raw frame pts; tracked for session 14 |

### Sidecar files next to a local video

Video `Movie.mkv`. Searched in the video folder, in `Subs`/`Sub`/`Subtitles`/`Subtitle` (any case) and in `Subs/Movie/`.

| File | Found | Language |
|---|---|---|
| `Movie.srt`, `movie.SRT` | yes (exact name is preferred) | — |
| `Movie.ru.srt`, `Movie.English.srt`, `Movie_en.srt`, `Movie [Forced].srt` | yes | from the suffix (`forced`, `sdh` are flags) |
| `Subs/Movie.en.srt`, `Subtitles/Movie.vtt` | yes | from the suffix |
| `Subs/Movie/2_English.srt` (any name inside a folder named after the video) | yes | from the name |
| `Other.srt`, `Movie2.srt`, `Extras/Movie.srt`, `Subs/Other/x.srt` | no | — |
| `.ass` / `.ssa` | found, not shown yet (session 08) | — |
| Non-UTF-8 file names | yes, paths are kept byte-exact | — |

## Controls (session 04)

| Feature | Status |
|---|---|
| CC button | right of Repeat, only when the video has at least one subtitle track; underlined when on. Window ≥ 498 pt: CC + ▾ menu; 470–497 pt: CC only (right-click opens the menu); < 470 pt: hidden, hotkeys still work |
| Menu | tracks (`Russian — forced (SRT, file)`; a file without a language shows its file name), Off, Load file…, Delay with Reset; unavailable tracks are greyed with the reason on hover |
| Hotkeys | `C` on/off, `V` next track (cycles through Off), `Z` / `X` delay −0.1 s / +0.1 s (one updating notification) |
| Auto-selection | off by default; when on: first track matching `subtitles.preferred_languages` (empty = system language; `rus` matches `ru`), otherwise the first track; forced tracks are shown even when subtitles are off |
| Remembered | whether subtitles are on (config `subtitles.enabled`); per-video track and delay memory: session 07 |
| Drop a subtitle file on the window | attaches to the current video (local or web) and shows it; the queue is not changed. Nothing playing → "Open a video first, then add subtitles." Video + subtitle file dropped together → attached to that video; dropped on the playlist panel → not attached (notification) |
| Language from a dropped / loaded file name | `Movie.ru.srt`, `sintel_ru.srt`, `Movie English.srt`, `Movie [Forced].srt`; only known languages (no video name to compare with, so `The_Big_Lie.srt` is not "lie") |
| Settings | Settings → `subtitles`: show subtitles, preferred languages, style (box / outline, size, colors, box opacity), resource limits (config schema v15) |

## Appearance and position (session 05)

| Feature | Status |
|---|---|
| Edit on video | Settings → `subtitles` → "Edit appearance on video…" (needs an open video and no unapplied settings changes). Works paused and while playing; shows the real cue, or a sample when none is on screen |
| Mouse | drag the frame = position (any direction), drag a corner or wheel over the video = text size; double-click and wheel do not toggle fullscreen / change volume while editing |
| Panel | text color, box color, box opacity, Box / Outline, Reset, Cancel, Done; Esc = Cancel (an open color picker closes first) |
| Saving | Done, reopening Settings or quitting saves in one config transaction; Cancel / Esc restores the previous style exactly |
| Position | stored as fractions of the picture (`position_horizontal_percent`, `position_vertical_percent`), so it survives window resize and fullscreen; never leaves the picture sideways; simple formats may sit on the black bar under the picture |
| Above the controls | subtitles move up only when the control panel would cover them, following the panel's slide animation |
| ASS / SSA | `ass_force_user_style` flag exists in settings; it takes effect with ASS rendering (session 08) |
| Known limit | the whole subtitle layer moves together, so a WebVTT cue placed by its author (e.g. at the top) moves with the user offset and with the lift above the controls |

## Text encodings (SRT, WebVTT files)

| Input | Result |
|---|---|
| UTF-8 with or without BOM | exact |
| UTF-16 LE/BE with BOM | exact |
| UTF-16 LE/BE without BOM | detected when the text contains Latin/ASCII (timings always do); pure non-ASCII UTF-16 without BOM is not detected |
| Legacy 8-bit (Windows-1251, KOI8-R, Windows-1250, Greek, CJK, …) | statistical guess (`chardetng`) with a Cyrillic preference hint; the result is reported so the UI can say "read as …" |

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
- text codecs: SubRip (`S_TEXT/UTF8`), WebVTT, ASS / SSA text lines, `mov_text`;
- every subtitle track of the video is read in the background, not only the selected one, so switching tracks during playback is instant and shows cues that were already read;
- seeking back shows previously read cues from the cache (duplicate packets are ignored);
- auto-select: preferred language, then non-SDH, then a file beside the video before an embedded track, then the container "default" flag, then list order; without a language match: a file beside the video, then the embedded default, then the first track. Forced tracks follow the same rules as before;
- fonts attached to MKV (`font/*`, `application/x-truetype-font`, `application/vnd.ms-opentype`, `application/font-sfnt`, `.ttf/.otf/.ttc/.otc`) are used for the video's subtitles; attachments over the limits (`max_attached_font_mb`, `max_attached_fonts_total_mb`) are not loaded and are reported in the log as counts only.

Limitations:
- picture tracks (PGS, VobSub, DVB) and unknown codecs are listed greyed out ("not supported yet");
- a cue without a duration in the container is shown for `embedded_cue_max_open_duration_ms` (default 5 s) instead of "until the next cue", because the next cue has not been read yet;
- WebVTT inside MKV carries no positioning in the packet (only the text and markup), so cues are placed by the user style;
- ASS / SSA tracks are drawn as plain text with the default style (override tags are stripped) until session 08; the track's own style header is not used yet;
- like mpv, cues are known only after the demuxer has read them: after a forward seek, a cue that started before the target and is still active appears once the demuxer reaches its packet (the container is read from the nearest keyframe before the target, so this is usually immediate); a cue that was never read is not shown;
- if the packet queue between the player and the UI overflows, packets are dropped (counted, one warning in the log) while video and audio keep playing.
