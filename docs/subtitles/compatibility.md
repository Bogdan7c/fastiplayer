# Fastiplayer: subtitle compatibility matrix (draft)

Status: **draft**, updated by each session of the subtitles plan (`user/subtitles/`). Final version — session 15.

Legend: **Full** — format features are parsed into the model; **Partial** — parsed with the listed gaps; **Planned** — scheduled in a later session; **Backlog** — not planned yet.

Session 01 delivers parsing only (crates `subtitle-core`, `subtitle-formats`). Nothing is shown on screen until session 03.

## Formats

| Format | Source | Status | Session |
|---|---|---|---|
| SubRip (`.srt`) | sidecar file | Partial (see below) | 01 |
| WebVTT (`.vtt`) | sidecar file | Partial (see below) | 01 |
| SubRip / WebVTT inside MKV | embedded | Planned | 06 |
| mov_text (`tx3g`) inside MP4 | embedded | Planned | 06 |
| ASS / SSA | sidecar, embedded | Planned (libass port) | 08–10 |
| PGS inside MKV | embedded | Planned | 11 |
| YouTube srv3 / WebVTT | web | Planned | 12 |
| HLS WebVTT, DASH TTML/IMSC | streaming | Planned | 13 |
| VobSub, DVB, FLV subtitles | — | Backlog | — |

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
| Fonts attached to MKV | interface only (`register_attached_font`); extraction — session 06 |
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
| Track choice | temporary: the first sidecar file found (menu and language preferences: session 04) |
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
