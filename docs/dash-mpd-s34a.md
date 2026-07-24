# S34A static DASH MPD profile

`dash-mpd-core` — чистый владелец XML schema/profile. Он принимает только
caller-owned bytes и обязательные `bounded-xml-reader` budgets. HTTP, effective
URL resolution, Representation selection, demux, seek и player lifecycle здесь
намеренно отсутствуют.

| Область | S34A |
|---|---|
| MPD namespace | Только exact `urn:mpeg:dash:schema:mpd:2011` |
| MPD profiles | Поле можно опустить. Exact allowlist приведён ниже; остальные typed rejected |
| Presentation | Только finite `type="static"`; Period обязаны образовать непрерывный timeline |
| BaseURL | Не более одного на MPD/Period/AdaptationSet/Representation; lexical inheritance chain сохраняется |
| SegmentTemplate | `$RepresentationID$`, `$Bandwidth$`, `$Number$`, `$Time$`, `$$`, bounded `%0Nd`; ровно `duration` либо `SegmentTimeline` |
| SegmentTimeline | `t`, positive `d`, `r >= -1`; `r=-1` раскрывается только до следующего `t` или caller-provided Period end; expansion caller-bounded и overflow-checked |
| SegmentList | Finite `SegmentURL`, optional Initialization, media/index ranges; uniform positive duration |
| SegmentBase | Optional `indexRange`, Initialization URL/range, timescale и presentation offset; S34B откроет это Range-backed source-ом |
| Containers | Только доказанные existing S28 paths: ISO BMFF/fMP4 и WebM |
| Components | Video-only, audio-only и muxed по согласованным MIME/contentType/codecs evidence |
| Multi-period | Bounded count, exact contiguous starts/durations, finite total duration |
| DRM | Любой `ContentProtection` typed rejected |
| XLink/foreign attributes | Typed rejected; external loading отсутствует |
| Dynamic/live/UTCTiming/EventStream/properties | Playback-changing unsupported constructs typed rejected |
| Неизвестные элементы/атрибуты | Fail-closed, без угадывания semantics |

Exact `profiles` allowlist:

- `urn:mpeg:dash:profile:full:2011`
- `urn:mpeg:dash:profile:isoff-on-demand:2011`
- `urn:mpeg:dash:profile:isoff-live:2011`
- `urn:mpeg:dash:profile:isoff-main:2011`
- `urn:mpeg:dash:profile:webm-on-demand:2012`

`service-ytdlp` остаётся extractor anti-corruption boundary и не знает MPD
schema. Его DASH accessor выбирает один input:

- non-empty concrete fragments — authoritative single-period serialization;
- `is_dash_periods=true` — только manifest-backed MPD, иначе typed reject;
- без fragments — `manifest_url`, затем selected `url`;
- fragment `url` обязан быть absolute HTTP(S) и имеет приоритет над `path`;
  `path` обязан быть relative same-origin reference (обычный либо root-relative),
  а scheme-relative/cross-origin reference rejected; relative path требует
  absolute HTTP(S) base;
- headers/cookies/query раскрываются только intent-named request-context API;
- generator/repr fragment state, invalid duration/filesize и secret-bearing
  diagnostics отвергаются до runtime.

S34B должен переиспользовать `web-media-adaptive`, S28A/S28B demux и
`source-core` URL/Range boundaries. Он не должен добавлять второй XML, MP4 или
WebM parser и не должен fallback-ить с authoritative fragments на MPD после
runtime ошибки.
