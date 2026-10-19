# Subtitles in Fastiplayer: user guide

Applies to version 0.2.0 (in `main`, not released yet). What each format and source supports is listed in the [compatibility matrix](compatibility.md).

## Where subtitles come from

Fastiplayer collects every subtitle track of the current video into one list:

| Source | How it gets into the list |
|---|---|
| **Files next to the video** | Found automatically when the video opens. For `Movie.mkv`: files whose name starts with `Movie` (`Movie.srt`, `Movie.ru.srt`, `Movie.English.ass`, `Movie [Forced].srt`) in the same folder, in a `Subs` / `Sub` / `Subtitles` / `Subtitle` folder (any case) and in `Subs/Movie/`. Extensions: `.srt`, `.vtt`, `.ass`, `.ssa`. The language comes from the name suffix |
| **Dropped file** | Drag a subtitle file onto the video while it plays: it is attached to that video and shown. You can also drop a video together with its subtitle file. Dropping it on the playlist panel does not attach it |
| **"Load file…"** | In the CC menu; same as dropping a file |
| **Inside the video** | MKV: SRT, WebVTT, ASS / SSA (with fonts attached to the MKV), PGS pictures. MP4: `mov_text`. The label says "embedded" |
| **YouTube** (through yt-dlp) | Manual subtitles, automatic captions `(auto)`, and auto-translations in the "Auto-translate" submenu. A track is downloaded only when you choose it |
| **HLS / DASH streams** | Subtitle tracks announced by the stream, labelled "(stream)". Only the shown track is downloaded, around the playback position |

## The CC button and menu

- **CC** sits right of the Repeat button. It appears only when the video has at least one subtitle track.
- **Click CC** to turn subtitles on or off. The button is underlined while they are on.
- **▾ next to CC** opens the menu: the track list, **Off**, **Load file…**, and **Delay** with **Reset**. In a narrow window only CC is shown, and a right-click on it opens the menu. In a very narrow window the button hides, but the hotkeys still work.
- **Greyed-out tracks** cannot be shown. Hover over one to see why: an unsupported format (for example VobSub), a file that could not be read, or a stream track that is unavailable right now.
- **Audio tracks:** when a video has two or more, a **♪ ▾** button appears left of CC. YouTube videos with dubbed audio list their audio languages there; the original comes first.

## Hotkeys

| Key | Action |
|---|---|
| `C` | Subtitles on / off |
| `V` | Next subtitle track (cycles through Off) |
| `Z` / `X` | Subtitle delay −0.1 s / +0.1 s. A positive delay shows subtitles later |
| `B` | Next audio track |

## Which track is chosen automatically

1. If you chose a track for this video before, that choice is restored, together with Off and the delay.
2. Otherwise, the first track in your **preferred languages** (Settings → subtitles; empty means the system language) is shown, but only when "Show subtitles" is on.
3. **Forced** tracks are shown even when subtitles are off. They translate foreign speech and signs. PGS pictures marked forced by the author are treated the same way.

Translations are never chosen automatically. Manual YouTube subtitles win over automatic captions in the same language.

The choice of audio track, subtitle track, Off and the delay is remembered **per video** and survives a restart. It is stored in `track-memory.json` next to the config, for up to 1000 videos by default.

## Changing the look on the video

Open Settings → **subtitles** → **Edit appearance on video…**. You need an open video and no unapplied settings changes.

- **Drag the frame** to move subtitles.
- **Drag a corner** or **use the mouse wheel** over the video to change the text size.
- The small panel sets the text colour, box colour and opacity, and **Box** or **Outline**. **Reset** returns to the defaults.
- **Done** saves. **Cancel** or **Esc** restores the previous look exactly.

The position is stored as a share of the picture, so it survives resizing and fullscreen. Simple subtitles may sit on the black bar under the picture. When the control panel appears, subtitles move up smoothly so they are not covered, and move back down when it hides (for example in fullscreen with auto-hide).

**ASS / SSA** keep the author's look and place. Your text size scales the dialogue. "Force my style" (`ass_force_user_style`) replaces the author's colours, outline and size with yours. **PGS** pictures keep the author's size and colours; only pictures in the lower half rise above the control panel. Neither ASS nor PGS is ever drawn on the black bars.

## When something goes wrong

Subtitle problems never stop the video.
- **A file that cannot be read** (missing, too large, no subtitles in it, not a regular file) gives one notification with the file name and the reason. Files found automatically show the reason only in the menu.
- **A YouTube limit** (HTTP 429) gives "YouTube is limiting subtitle requests. Try again in a minute." The track stays in the menu; click it again to retry.
- **A stream track whose segments keep failing** gives "Subtitles for this stream are unavailable right now." Short network outages, up to `network.reconnect_wait_ms`, are bridged silently.

## Settings

All keys live in `config.toml` (schema v23) and in Settings → **subtitles**. Limits and caches take effect with the next opened video; `[subtitles.stream]` takes effect when the next stream track starts.

### `[subtitles]`

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Show subtitles automatically (CC and `C` change it) |
| `preferred_languages` | `""` | Priority list such as `"uk, ru, en"`; empty means the system language |
| `decoration` | `box` | `box` (YouTube-like dark box) or `outline` |
| `text_size_percent` | `100` | 50–250 |
| `text_color`, `background_color` | `#FFFFFF`, `#080808` | `#RRGGBB` |
| `background_opacity_percent` | `75` | 0–100 |
| `position_horizontal_percent`, `position_vertical_percent` | `50`, `6` | Position in the picture; a negative vertical value (down to −40) puts subtitles on the bar below the picture |
| `ass_force_user_style` | `false` | Apply your style to ASS / SSA too |
| `ass_position_mode` | `main_lines_avoid_controls` | ASS placement: `main_lines_avoid_controls`, `author` (nothing moves) or `whole_layer` (moves like SRT, stays inside the picture) |
| `max_file_size_mb`, `max_cues_per_file`, `max_chars_per_cue` | `8`, `100000`, `2000` | Limits for subtitle files and cues |
| `sidecar_max_entries_per_folder`, `sidecar_max_candidates` | `5000`, `32` | Limits of the search next to the video |
| `max_cues_on_screen`, `max_ass_events_on_screen` | `16`, `256` | Cues drawn at once |
| `max_attached_font_mb`, `max_attached_fonts_total_mb` | `20`, `100` | Fonts attached to MKV |
| `embedded_feed_capacity`, `embedded_cue_max_open_duration_ms` | `4096`, `5000` | Embedded packets waiting for the UI; display time of an embedded cue without a duration |
| `max_ass_header_kb`, `max_ass_styles`, `max_ass_override_tags_per_event` | `1024`, `1000`, `512` | ASS limits |
| `render_cache_mb`, `font_lookup_cache_chars`, `glyph_cache_mb`, `glyph_outline_cache_items`, `shaping_cache_mb`, `blur_scratch_retained_mb` | `32`, `65536`, `64`, `10000`, `4`, `32` | Rasterizer memory |
| `prerender_frames`, `prepared_overlays` | `4`, `8` | How far ahead the subtitle render thread prepares pictures |
| `pgs_displayed_track_cache_mb`, `pgs_background_track_window_s` | `32`, `60` | PGS picture memory |

### `[subtitles.stream]` (HLS / DASH)

| Key | Default | Meaning |
|---|---|---|
| `ahead_s`, `behind_s` | `60`, `30` | Seconds of subtitles fetched ahead of and behind the playback position |
| `max_segment_kb` | `1024` | Larger subtitle segments are skipped |
| `max_pending_segments` | `8` | Segments waiting for the video time reference |
| `failure_threshold` | `3` | Failed segments in a row before the track is reported unavailable |
| `live_retention_s` | `600` | Seconds of cues kept behind the position on live streams |
