# 14. Manual Seek/Scrub Acceptance

Дата: 2026-05-21.

Документ описывает ручную acceptance-проверку реального playback после работ по
seek/scrub recovery. Проверка намеренно живёт вне runtime-кода: `player-core`
продолжает владеть state machine и structured diagnostics, а checklist и parser
только читают опубликованные log markers.

## Архитектура Проверки

Владение состоянием:

- `player-core::PlayerSession` владеет seek transaction, seek generation,
  audio/video gates, drop counters и `ActiveSeekDiagnosticsSnapshot`.
- `symphonia-demux` владеет container seek и `TracksChanged` после
  `ResetRequired`.
- `app-egui` владеет UI-сценарием: click seek, drag, release и пауза.
- `scripts/parse-seek-diagnostics.py` не принимает playback-решений и не
  читает private fields. Он связывает строки логов в одну таблицу по уже
  опубликованным fields: `generation`, `target_ms`, `kind`, `blocker`,
  `*_pts_ms`, drop taxonomy.

Boundary markers, которые нельзя молча переименовывать без обновления этого
документа и parser-а:

- `Starting demux seek transaction`
- `Demux seek transaction accepted`
- `Post-seek demux packet observed`
- `First post-seek video packet observed`
- `First post-seek decoded frame observed`
- `First post-seek presented frame observed`
- `Final seek commit завершён`
- `Active seek transaction is still waiting`
- `Demuxer сообщил обновление track list`
- `Active seek rebased after post-seek TracksChanged/ResetRequired marker`

Инварианты acceptance:

- Final seek закрывается событием `Final seek commit завершён`; если нет commit,
  parser должен показать текущий `blocker`.
- Preview seek не считается fresh без `First post-seek presented frame observed`
  или визуально подтверждённого live preview.
- `late` и `queue_overflow` во время seek считаются normal playback drops и
  требуют расследования. `seek_preroll` и `stale_generation` считаются отдельно.
- `TracksChanged` внутри active seek допустим только если следом есть rebase
  marker и новые packets идут в актуальную generation.

## Запуск Логов

Локальный файл:

```bash
RUST_LOG=player_core=debug,symphonia_demux=debug,app_egui=debug \
cargo run -p app-egui -- /path/to/media.webm 2> /tmp/rustiplayer-seek.log
```

YouTube VOD:

```bash
RUST_LOG=player_core=debug,symphonia_demux=debug,app_egui=debug \
cargo run -p app-egui -- 'https://www.youtube.com/watch?v=VIDEO_ID' \
  2> /tmp/rustiplayer-youtube-seek.log
```

Разбор лога:

```bash
scripts/parse-seek-diagnostics.py --scenario "VP9 SDR: seek near EOF" \
  /tmp/rustiplayer-seek.log
```

CSV/JSON для таблицы:

```bash
scripts/parse-seek-diagnostics.py --format csv /tmp/rustiplayer-seek.log
scripts/parse-seek-diagnostics.py --format json /tmp/rustiplayer-seek.log
```

## Media Coverage

| ID | Media | Источник | Что покрывает | Результат |
| --- | --- | --- | --- | --- |
| M1 | Local VP9 SDR WebM | `*.webm` | VP9 Profile 0, NV12, video-only или A/V | |
| M2 | Local VP9 HDR/P010 WebM | `*.webm` | VP9 Profile 2, P010, HDR-to-SDR path | |
| M3 | Local MKV/WebM audio+video | `*.mkv` / `*.webm` | selected audio gate, A/V resume | |
| M4 | Audio-only file | `*.opus` / `*.mka` / другой audio | no selected video, audio-only seek | |
| M5 | YouTube VOD | URL | service-youtube + dual stream demuxer | |

## Scenario Matrix

Каждый сценарий фиксируется отдельной строкой результата. Если один запуск
проверяет несколько сценариев, parser всё равно выводит одну строку на каждый
seek transaction.

| ID | Сценарий | Обязательные media | Действие | Acceptance |
| --- | --- | --- | --- | --- |
| S1 | Ordinary final seek | M1, M2, M3, M5 | Click-to-seek в середину timeline | Есть demux accepted, packet, decoded frame, presented frame, final commit |
| S2 | Seek near EOF | M1, M2, M3, M5 | Seek в последние секунды media | Нет indefinite `Seeking`; EOF fallback допустим только как свежий fallback |
| S3 | Seek near beginning | M1, M2, M3, M5 | Seek в первые секунды media | Generation не orphan-ится, playback продолжает работу |
| S4 | Multiple rapid seeks | M1, M3, M5 | Быстро выполнить 3-5 click seeks | Latest target wins; старые generations не становятся visible |
| S5 | Slow drag | M1, M2, M3 | Медленно вести pointer по timeline | На каждое заметное движение появляется fresh preview |
| S6 | Fast drag | M1, M3, M5 | Быстро провести pointer по timeline | Worker не забивается; нет normal playback drops |
| S7 | Release immediately after drag start | M1, M3 | Begin drag и сразу release | Если preview не был виден, final seek идёт в latest target |
| S8 | Pause -> seek -> remains paused | M1, M3, M4 | Pause, затем seek | Commit закрывается, итоговый state остаётся paused |
| S9 | Playing -> seek -> resumes playing | M1, M2, M3, M5 | Playback playing, затем seek | Commit закрывается, итоговый state возвращается playing |
| S10 | Audio-only seek | M4 | Seek в середину audio-only | Video gate не блокирует; audio gate даёт понятный blocker или commit |

## Results Table

| Run | Media | Scenario | Seek start | Demux accepted | First packet | First decoded | First presented | Final commit | Blocker >250ms | Drops seek/stale/late/queue | Audio gate | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| | | | | | | | | | | | | |

Минимальные правила verdict:

- `PASS`: final/preview цель достигнута, нет stale frame after commit, нет
  `late`/`queue_overflow` во время seek.
- `WARN`: commit есть, но были `stale_generation`, audio soft fallback или
  blocker дольше 250 ms.
- `FAIL`: нет demux accepted, нет post-seek packet, нет decoded/presented frame
  для video media, нет final commit, либо UI остался в `Seeking`.

## Regression Patterns

| Pattern | Быстрая интерпретация | Следующий модуль |
| --- | --- | --- |
| `Starting demux seek transaction` без `Demux seek transaction accepted` | Demux seek не принят или вернул typed error | `player-core` -> demux boundary |
| `Demux seek transaction accepted` без `Post-seek demux packet observed` | Demux/read loop не отдаёт packets после seek | `symphonia-demux`, `dual_stream_demuxer` |
| `Demuxer сообщил обновление track list` без rebase marker | Возможен orphan active seek после `ResetRequired` | `PlayerSession::handle_demux_track_list_update` |
| `First post-seek video packet observed` без decoded frame | Decoder bootstrap/input/keyframe path | `tick`, decoder boundary |
| decoded frame есть, presented frame отсутствует | Scheduler/admission/render lease pressure | `tick`, `PlaybackPipeline`, render lease bridge |
| presented frame есть, commit отсутствует | Gate blocker, чаще audio или video resume preroll | `seek_audio_gate_status`, `seek_commit_gate_decision` |
| `blocker=audio_*` дольше 250 ms | Video уже может быть готов, но audio gate держит commit | audio decoder/output/preroll |
| `blocker=post_flush_keyframe` и растёт `dropped_until_keyframe` | Keyframe probe или seek decode point неверен | `symphonia-demux`, packet mapper, codec probe |
| `drops_late` или `drops_queue` растёт во время seek | Seek вызвал normal playback drops | scheduler/backpressure |
| `drops_stale_generation` растёт без active reset/rebase | Generation mismatch | session/pipeline generation boundary |

## Parser Contract

`scripts/parse-seek-diagnostics.py` читает stderr/stdout логов и строит таблицу.
Он намеренно tolerates partial logs: незакрытый seek остаётся строкой со статусом
`FAIL` и последним известным blocker-ом. Это нужно для зависаний, где именно
отсутствующее событие является результатом проверки.

Parser не доказывает визуальный UX drag сам по себе. Для slow drag нужно
сравнить количество preview seek строк с фактическими pointer movements и
убедиться, что свежий кадр был виден глазами.
