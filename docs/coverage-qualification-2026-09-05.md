# S12: квалификация stable coverage, 2026-09-05

Этот отчёт фиксирует локальную квалификацию исправленного экспорта на исходниках
`c8bd926b09819db8b040f9d89338db5a2c7a73b2`. Он не переносит результат на другой SHA
и не заменяет удалённые release gates. После установки baseline обязательны два
свежих `scripts/coverage.sh check`; tag/release требуют полного успешного удалённого CI
на окончательном public main SHA.

Первый свежий check на `160d34bc065126a4dc3f2cbfb6d902b70c99971f` выявил
две нестабильные ветки при неизменном universe: отмена внутри ordered-resource
чтения MPEG-TS и отмена уже armed adaptive HTTP body. Все три test execution
прошли, но ratchet отклонил результат. Добавлены детерминированные consumer tests:
источник отменяет запрос перед возвратом ошибки; HTTP fixture отменяется только
после подтверждения active read владельцем состояния. Baseline не понижен,
эти координаты остаются обязательными. После этих тестовых изменений нужны
два новых check и удалённая квалификация окончательного SHA.

Следующий check на `31d0d777f036a0fb28792eec200c202162cefbef` сохранил обе
ветки отмены, но обнаружил три нестабильные строки ожидающего metadata probe.
Тест теперь отдельно воспроизводит преждевременные notifications: два rendezvous
сообщения гарантируют наблюдение `Probing` до release worker-а, затем consumer
применяет настоящий prepared sort и metadata patch. Это добавление теста также
не понижает baseline; допуск требует новых полных checks.

На `576194800ea9cb981ae3dabb34ee59d191cde6ab` первый полный check прошёл,
а второй сохранил предыдущие исправления, но отклонил ранний shutdown FFmpeg
worker (`worker.rs:38`). Новые consumer lifecycle tests заранее отправляют
shutdown либо закрывают его канал до запуска настоящего owner-loop и проверяют
завершение без обработки queued packet и без публикации frame/error/completion.
Baseline сохранён. Автоматический CI 18/18 и Toolchain 3/3 этого SHA успешны,
но не квалифицируют следующий SHA; два локальных check и полный удалённый CI
вновь обязательны перед выпуском.

## Причина и исправление

LLVM 22.1.2 multi-object export воспроизводимо показывал 0 или 6 вызовов одной
функции `ChromaSubsampling::from_matroska_subsampling` при перестановке двух
неизменных binaries и том же merged profile. Отдельный export показывал 6.
Исправление сохраняет каждый single-object JSON, проверяет frozen executable/profile
identity и объединяет exact coordinate sets. Полный wrapper export остаётся legacy
report-only; новая реализация отмечена в `cohort-manifest.coordinate_export`.

Дополнительно стабилизирован настоящий cancellation wait/wake test и расширены
consumer tests для ориентации, ошибки/отмены источника после настоящих media packets
и влияния Matroska chroma на совместимость декодера. Source exclusions, normal
concurrency, определения координат и трёхпрогонное пересечение не ослаблены.

## Девять measured runs

Три независимых build/cohort, по три успешно завершённых workspace test execution.
Во всех совпадают source/tool/profile/coordinate universes; внутри каждого cohort
executable hashes неизменны. Разные ELF hashes между отдельными сборками допустимы
и не выдаются за reproducible-build proof.

- Rust `1.96.0`; LLVM `22.1.2`; cargo-llvm-cov `0.8.7`.
- `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_TEST_DEBUG=0`, обычная test concurrency.
- Source inventory: `6bc541cae84e8be6cbb32cd82cbd88a4bd84bd24fde70d924fadb1a7cd3f9723`, 2344 файлов.
- В baseline включено только точное пересечение stable sets всех девяти runs.

| Cohort | Cohort SHA-256 | Executable inventory SHA-256 |
| --- | --- | --- |
| 1 | `sha256:895a6deb1a12550187aa8639fab0c0fbcd576dfe0cfcd4666d8d08ac61d46b6d` | `b1f6013b80f99c96eaf6f17fda5089cb2f76afba1aae106091f4b71f5ba78d08` |
| 2 | `sha256:160c27dee79349b29ca795488b37df7a88631c6535cea7c0516c9a25e7546b64` | `4727f9ee0a4875425bc701c9662a9bd81a09413c7c1fceddeac4a4c57e8b505b` |
| 3 | `sha256:8ec866e766859471b52e580f3ac7d5c314f180df0cf1c0df53937580a2519f87` | `78f4d485527f59ff1ec8f53a67f5495caac32e9b00e7ec0b409b43c999f7b514` |

Run-state hashes (порядок: cohort 1/run 1–3, затем cohort 2 и 3):

- `sha256:ddc057939dc6a1bf514be5850f31ee248d54217f621c9c6ad3b71fe7fdfaaf0f`
- `sha256:395037a04aebe6d0170bd402319543231ae2a7fdeeb1b862a5f02635080e0d56`
- `sha256:c6af5067ff1adfa33f1f6bd021d58588839da52ba0324fbb11c06bb031d13394`
- `sha256:c821fbb9006fa779c83c46e66f497b1228e54c9dfd666adceb416f9d4dd2c37f`
- `sha256:d192df7a7a348d68965ce6df02aee55b3239a0efa551d8298ce7bd2c607490f9`
- `sha256:22a74413c15bdcae32859981127d06cbb030ac6d495d46301e62414461bfbaea`
- `sha256:c8a16da43178088baee6323b2cd0c5b24bdced0a8d37bd40fbf167abd8e56937`
- `sha256:f01111660fbed5b8217defe1f7c5c781830873d9dc3e0599c430e5a05ff288f9`
- `sha256:6acbffb4404c6da14d76338b3083376865e87cd2a16f7a47a675a03519fc9a47`

Полученный baseline: `sha256:f2a813b52ec90dc995a21737563cf9378b7ccb1514b6e6fd6fb51ea95e45cd49`.
Предыдущий baseline: `sha256:ed3320f87c1c353f89aeeb181fa2cbf20f057dbe83e95032e5080119d700773a`.
Legacy v1 provenance сохранена без изменения.

## Проверка по файлам

Сравнение выполнено с baseline revision `f3a6ef71`; оно учитывает S08 изменения
и S12 исправления тестов. В неизменных domain universes прежние exact stable
coordinates сохранены. Таблица показывает все файлы с изменившимися координатами
или counts и присутствующие в coverage изменённые исходники. Значения — stable/total.

| Файл | Lines: прежде → 9 runs | Functions: прежде → 9 runs | Regions: прежде → 9 runs |
| --- | --- | --- | --- |
| `crates/app-egui/src/frame_prepare/submit.rs` | 0/51 → 0/57 | 0/1 → 0/1 | 0/69 → 0/79 |
| `crates/app-egui/src/media_open/executor.rs` | 319/385 → 318/384 | 34/38 → 34/38 | 468/573 → 473/578 |
| `crates/app-egui/src/web_media_open/content_probe_tests/direct_progressive_webm.rs` | 335/358 → 335/358 | 17/17 → 17/17 | 443/474 → 443/474 |
| `crates/codec-core/src/model.rs` | 450/515 → 459/515 | без изменения | 577/656 → 586/656 |
| `crates/media-core/src/seek_cancellation.rs` | 345/354 → 332/341 | 35/35 → 34/34 | 580/592 → 556/569 |
| `crates/media-prefetch/src/buffer.rs` | 251/251 → 251/251 | 31/31 → 31/31 | 465/465 → 465/465 |
| `crates/media-prefetch/src/seek.rs` | 38/40 → 38/40 | 1/1 → 1/1 | 50/52 → 50/52 |
| `crates/media-prefetch/src/shared.rs` | 75/75 → 83/83 | 12/15 → 12/15 | 79/85 → 94/100 |
| `crates/media-prefetch/src/source.rs` | 600/625 → 600/625 | 65/67 → 65/67 | 893/928 → 894/929 |
| `crates/mpeg-ts-demux/src/framing.rs` | 316/387 → 317/387 | без изменения | 432/489 → 433/489 |
| `crates/player-core/src/worker.rs` | 97/199 → 97/199 | 12/22 → 12/22 | 75/176 → 75/176 |
| `crates/player-core/src/worker/handle.rs` | 168/260 → 172/264 | 19/32 → 19/32 | 198/287 → 208/297 |
| `crates/player-core/src/worker/runtime_commands.rs` | 253/351 → 254/352 | 17/18 → 17/18 | 310/433 → 312/435 |
| `crates/player-core/src/worker/runtime_publish.rs` | 92/224 → 97/229 | 11/21 → 11/21 | 120/324 → 127/331 |
| `crates/playlist-core/src/entry.rs` | 257/352 → 256/352 | без изменения | 292/419 → 291/419 |
| `crates/playlist-core/src/queue/removal.rs` | 151/164 → 138/151 | 12/13 → 9/10 | 196/215 → 175/194 |
| `crates/render-wgpu-shell/src/shell.rs` | 26/334 → 26/342 | 3/29 → 3/29 | 32/425 → 32/430 |
| `crates/symphonia-demux/src/presentation_window_ordered.rs` | 325/352 → 326/352 | без изменения | 394/456 → 395/456 |
| `crates/video-vaapi/src/decoder_thread.rs` | 86/434 → 86/436 | 9/30 → 9/30 | 93/583 → 93/583 |
| `crates/web-media-adaptive/src/streaming_resource.rs` | 292/306 → 293/306 | без изменения | 354/369 → 355/369 |

Изменённые test source paths вне default LLVM source report (тестовые binaries
при этом исполняются; новых exclusions не добавлялось):

- `crates/app-egui/src/media_open/web/tests/native_cross_source_playlist.rs`
- `crates/app-egui/src/media_open/web/tests/native_dash_live_vertical.rs`
- `crates/app-egui/src/media_open/web/tests/native_dash_vertical.rs`
- `crates/app-egui/src/media_open/web/tests/native_hds_vertical.rs`
- `crates/app-egui/src/media_open/web/tests/native_hls_lifecycle_n14b.rs`
- `crates/app-egui/src/media_open/web/tests/native_hls_live_vertical.rs`
- `crates/app-egui/src/media_open/web/tests/native_hls_vertical.rs`
- `crates/app-egui/src/media_open/web/tests/native_smooth_vertical.rs`
- `crates/app-egui/src/web_media_open/content_probe_tests.rs`
- `crates/codec-core/tests/matroska_decode_requirement.rs`
- `crates/media-prefetch/src/source/tests/active_fetch.rs`
- `crates/player-core/src/worker/staged_media_install/tests.rs`
- `crates/player-core/src/worker/staged_media_install/tests/snapshot_publication.rs`
- `crates/player-core/src/worker/tests.rs`
- `crates/player-core/src/worker/tests/snapshot_read.rs`
- `crates/playlist-core/src/queue/removal/tests.rs`
- `crates/symphonia-demux/src/presentation_window_ordered/tests.rs`
- `crates/symphonia-demux/src/presentation_window_ordered/tests/source_failure.rs`
- `crates/symphonia-demux/src/symphonia_demuxer/tests.rs`
- `crates/web-media-http/src/tests.rs`

Результат ручного разбора:

- `media-core/seek_cancellation.rs`: уменьшение только в inline cfg(test);
  spin loop и canceller closure заменены rendezvous. Production wait/cancel не изменён.
- `playlist-core/queue/removal.rs`: удалён квадратичный test-only helper; линейный
  тест сохраняет identity и payload-sharing проверки всех 50000 rows. Единственная
  потеря в неизменном файле — cfg(test) no-match branch `entry.rs:623`, которая больше
  не вызывается удалённым поиском. Production queue behavior не потерян.
- Автоматический line matcher сопоставляет старую закрывающую скобку
  `worker/handle.rs:289` с новой `:295`. Реальная скобка прежнего цикла — `:294`
  и остаётся stable; `:295` закрывает новую lock scope и не входит в executable universe.
- Prefetch и snapshot-publication additions имеют functional coverage; сохранены
  прежние live paths после переноса/сдвига строк.
- Informational UI/GPU additions (surface resize, presentation diagnostics и
  VA-API diagnostic statements) остаются в denominator без новой hardware-квалификации.
- Восстановленное codec coverage не разрешается exception-ами: оно доказано
  выполнением и присутствует в exact nine-run intersection.

## Exact переход baseline

`check-baseline-update` для предыдущей и предложенной пары baseline/ledger: **PASS**.
Same-universe stable loss не разрешён. Ровно следующие cross-universe fraction
changes потребляют новые bounded rows; прежние rows не переносятся как разрешения
на будущее. Причины и follow-up записаны в `coverage/measurement-exceptions.json`
с review deadline `2026-12-04`.

| Domain / metric | Прежние stable/total | Разрешённые stable/total |
| --- | --- | --- |
| `blocking-group/functions` | 9917/11760 | 9913/11756 |
| `crate:media-core/lines` | 2017/2111 | 2004/2098 |
| `crate:media-core/functions` | 229/247 | 228/246 |
| `crate:media-core/regions` | 2798/2926 | 2774/2903 |
| `crate:playlist-core/lines` | 4891/6276 | 4877/6263 |
| `crate:playlist-core/functions` | 567/719 | 564/716 |
| `crate:playlist-core/regions` | 5962/8012 | 5940/7991 |
| `workspace/lines` | 164686/212402 | 164688/212409 |
| `workspace/functions` | 15800/20033 | 15796/20029 |
| `workspace/regions` | 206815/270182 | 206821/270193 |

Этот transition учитывает удаление полностью покрытого тестового кода и добавление
informational hardware/UI source. Он не исключает исходники, не меняет список
blocking owners, не разрешает терять прежние stable coordinates в неизменном universe
и не заменяет два свежих check после установки или final-SHA удалённую квалификацию.
