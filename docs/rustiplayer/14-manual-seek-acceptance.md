# 14. Manual Seek/Scrub Acceptance

Дата: 2026-05-23.

Документ описывает ручную acceptance-проверку реального playback после работ по
seek/scrub recovery. Проверка намеренно живёт вне runtime-кода: `player-core`
продолжает владеть state machine и structured diagnostics, а checklist и parser
только читают опубликованные log markers.

Текущий статус seek/scrub:

- ordinary click seek остаётся активным сценарием и должен закрываться обычным
  final seek commit-ом;
- drag release временно работает как простой final seek в позицию release через
  тот же normal seek path;
- live drag preview удалён из текущего runtime и будет переписан позже с нуля,
  поэтому acceptance больше не требует preview transaction markers.

## Архитектура Проверки

Владение состоянием:

- `player-core::PlayerSession` владеет seek transaction, seek generation,
  audio/video gates, drop counters и `ActiveSeekDiagnosticsSnapshot`.
- `symphonia-demux` владеет container seek и `TracksChanged` после
  `ResetRequired`.
- `app-egui` владеет UI-сценарием: click seek, локальный transient drag state,
  release-to-final-seek и пауза.
- `scripts/parse-seek-diagnostics.py` не принимает playback-решений и не
  читает private fields. Он связывает строки логов в одну таблицу по уже
  опубликованным fields: `generation`, `target_ms`, `kind`, `blocker`,
  `*_pts_ms`, drop taxonomy. Parser не ищет старые preview-only markers.

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
- Drag release закрывается тем же final seek contract-ом, что и click seek:
  demux accepted -> packet -> decoded/presented frame для video media -> final
  commit.
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

| ID | Статус | Сценарий | Обязательные media | Действие | Acceptance |
| --- | --- | --- | --- | --- | --- |
| S1 | Active | Ordinary click final seek | M1, M2, M3, M5 | Click-to-seek в середину timeline | Есть demux accepted, packet, decoded frame, presented frame, final commit |
| S2 | Active | Seek near EOF | M1, M2, M3, M5 | Seek в последние секунды media | Нет indefinite `Seeking`; EOF fallback допустим только как свежий fallback |
| S3 | Active | Seek near beginning | M1, M2, M3, M5 | Seek в первые секунды media | Generation не orphan-ится, playback продолжает работу |
| S4 | Active | Multiple rapid click seeks | M1, M3, M5 | Быстро выполнить 3-5 click seeks | Latest final target wins; старые generations не становятся visible |
| S5 | Removed / pending rewrite | Slow drag live preview | M1, M2, M3 | Медленно вести pointer по timeline | Не входит в текущий PASS; live preview удалён и не должен требовать preview markers |
| S6 | Removed / pending rewrite | Fast drag live preview | M1, M3, M5 | Быстро провести pointer по timeline | Не входит в текущий PASS; replacement/in-flight preview behavior будет проектироваться заново |
| S7 | Active | Drag release simple final seek | M1, M2, M3, M5 | Begin drag, вести pointer, release | На release отправляется normal final seek в latest pointer target; acceptance как у S1 |
| S8 | Active | Release immediately after drag start | M1, M3 | Begin drag и сразу release | Final seek идёт в latest release target без preview precondition |
| S9 | Active | Pause -> seek -> remains paused | M1, M3, M4 | Pause, затем seek | Commit закрывается, итоговый state остаётся paused |
| S10 | Active | Playing -> seek -> resumes playing | M1, M2, M3, M5 | Playback playing, затем seek | Commit закрывается, итоговый state возвращается playing |
| S11 | Active | Audio-only seek | M4 | Seek в середину audio-only | Video gate не блокирует; audio gate даёт понятный blocker или commit |

## Future Scenarios

S5/S6 сохраняют номера как зарезервированные сценарии будущего live preview
rewrite. До новой архитектуры они не являются acceptance PASS/FAIL критериями и
не должны добавлять требования к parser-у или текущим runtime log markers.

## Results Table

| Run | Media | Scenario | Seek start | Demux accepted | First packet | First decoded | First presented | Final commit | Blocker >250ms | Drops seek/stale/late/queue | Audio gate | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| | | | | | | | | | | | | |

Минимальные правила verdict:

- `PASS`: final цель достигнута, нет stale frame after commit, нет
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

Parser проверяет только текущий final seek contract. Он намеренно не ищет
preview transaction markers, stale preview replacement, visible-preview
promotion или in-flight preview replacement, потому что старый live preview core
удалён.
