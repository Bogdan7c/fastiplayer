# browser-bridge-bench — стенд проверки моста «браузер → плеер»

Это тестовый стенд, а не часть продукта. Он отвечает на один вопрос: **если браузер качает
видео, доходит ли оно до нашего плеера и показывается ли там кадрами** — и делает это сам,
без участия владельца, на невидимом экране.

## Одна команда

```bash
scripts/browser-bridge-bench.sh all
```

Скрипт собирает плеер, посредника и сам стенд, собирает настоящий экстеншен
(`extensions/browser`) и запускает проверки. Любую команду и любые флаги можно передать после
имени скрипта: `scripts/browser-bridge-bench.sh smoke --browser firefox`.
`scripts/browser-bridge-bench.sh --help` показывает все опции.

Код выхода: `0` — всё PASS (`unsupported` там, где он заранее разрешён, не считается
провалом), `1` — что-то провалилось, `2` — ошибка в аргументах. В конце печатается таблица и
время каждого шага.

## Быстро: `quick`, `--jobs` и «проверяем только задетое» (сессия 16b3c2, решение 386)

Полный прогон `all` шёл больше двух часов. Теперь по умолчанию гоняется **быстрый набор и
то, что задела правка**; `all` — только перед вехой.

```bash
scripts/browser-bridge-bench.sh quick                  # быстрый набор, оба браузера (~2 мин)
scripts/browser-bridge-bench.sh capture --page feed     # задетое по таблице ниже
scripts/browser-bridge-bench.sh all                     # перед вехой (частями, --jobs по умолчанию)
```

Что сделало стенд быстрым:

- **Короткая сводка плеера.** Стенд ставит плееру тестовую переменную
  `FASTIPLAYER_TEST_BROWSER_SUMMARY_INTERVAL_MS=1000`: сводка `Browser playback summary` идёт
  раз в 1 с вместо 10 (обычный плеер — прежние 10 с; плеер принимает 250 мс … 10 с). Судьи
  роста кадров не считают сводки: правило — «не меньше N кадров не дольше чем за 10 с по
  часам плеера» (`observe/frame_growth.rs`, время — отметки самих строк журнала).
- **Части в отдельных процессах.** `--jobs N` гонит до N частей сразу (часть — команда ×
  браузер × страница); каждая часть — дочерний процесс стенда со своим сайтом, журналом
  запросов сервера и песочницей, поэтому улики соседей в неё не попадают. По умолчанию
  N = процессоры / 2, не больше 8; с `--display real` — 1. Таблица — в порядке частей,
  прогресс `[k/n]` — в stderr, журнал упавшей части остаётся в
  `target/browser-bridge-bench/work/jobs-*/`. Ctrl-C родителя пересылается детям (SIGTERM),
  и каждый убирает свои KWin, шину и браузер.
- **Плеер для стенда — release.** Скрипт собирает плеер и посредника с оптимизацией (так
  требует план 16b3c2: строки по времени под нагрузкой параллельных частей не должны зависеть
  от скорости отладочной сборки; отладочный плеер под такой нагрузкой не замерялся);
  отладочный — `BRIDGE_BENCH_PLAYER_PROFILE=debug`.

### Быстрый набор `quick`

Список утвердил владелец (16 строк × 2 браузера). Каждая строка — своя часть; где на странице
несколько строк, часть выбирает одну отбором `BRIDGE_BENCH_*_ROW`, и часть обязана выдать
ровно ожидаемые строки (иначе FAIL «the part was expected to run …»).

| # | Что | Команда и строка |
|---|---|---|
| 1 | MSE fMP4 (H.264, как YouTube) + контроль линейки | `capture` `mse-fmp4` |
| 2 | MSE WebM (VP9/Opus, как YouTube) | `capture` `mse-webm` |
| 3 | Смена качества на ходу + контроль | `capture` `mse-abr` |
| 4 | Реклама → фильм в том же элементе (как YouTube) | `capture` `ad-film-same-element` |
| 5 | Реклама отдельным роликом → фильм | `capture` `ad-preroll` |
| 6 | Пауза, play, перемотка в буфер и за буфер, закрытие окна | `player-commands` `mse-fmp4` |
| 7 | Файл `<video src>` | `capture` `video-src` |
| 8 | Родной HLS в `src` (в Firefox — unsupported) | `capture` `m3u8-in-src` |
| 9 | Следующая серия на той же странице | `episodes` `delayed-next` |
| 10 | Следующая серия в запомненной озвучке | `series-memory-autoplay` |
| 11 | Перезагрузка страницы → то же окно | `page-reload` `reload-same-url` |
| 12 | Возврат закрытой вкладки в тот же сеанс | `tab-return` `return-mse` |
| 13 | Режим 2: клик по видео — и оно в плеере | `auto-handoff` `click-hands-off` |
| 14–16 | Ленты: много `<video>`, пул из двух, один переносимый элемент | `capture` `feed-many`, `feed-pool`, `feed-moved` |

Отбор одной строки руками (то же делает `quick`): `BRIDGE_BENCH_FEED_ROW`,
`BRIDGE_BENCH_AUTO_HANDOFF_ROW`, `BRIDGE_BENCH_TAB_RETURN_ROW`, `BRIDGE_BENCH_RETURN_ROW`,
`BRIDGE_BENCH_ENTRY_ROW` — часть имени строки; `BRIDGE_BENCH_EPISODES_ROW`,
`BRIDGE_BENCH_PAGE_RELOAD_ROW`, `BRIDGE_BENCH_SERIES_MEMORY_ROW`, `BRIDGE_BENCH_AD_HOP_ROW` —
часть имени или точное имя через `=` (`=delayed-next` не возьмёт `delayed-next-hidden`).

### Протокол «только задетое»

Перед коммитом: юнит-тесты задетого (`npm run check` в `extensions/browser`, `cargo test` по
задетым crate-ам), **быстрый набор** и команды из таблицы по задетым путям. Полный `all` —
только перед вехой. Путь не нашёлся в таблице — быстрый набор плюс команда, чей сценарий
проверяет задетое поведение, а строка в таблицу дописывается.

| Что правил | Что гонять сверх `quick` |
|---|---|
| Экстеншен `src/page/` (перехват MSE, `mse-hook`, «память без копий» `journal/`, `segment-scanner/`) | `capture`, `page-ends`, `attacks` |
| Экстеншен `src/page/shadow/` (тень, часы, `shadow-assembly.ts`) | `capture`, `page-ends`, `player-commands`, `page-return`, `tab-return`; эфир — ещё `hls-live`, `live-ends` |
| Экстеншен `src/page/next-video-guard.ts`, `background/handoff/video-relocation.ts` (переезд) | `capture --page ad-preroll --page ad-chain --page ad-film-same-element --page feed`, `episodes` |
| Экстеншен `background/handoff/feed/`, строки лент | `capture --page feed --page feed-hover` и остальные `feed-*`, `auto-handoff --page auto-feed` |
| Экстеншен `background/handoff/` (сеанс, `session-*.ts`, `frame-survey.ts`, `hand-off-command.ts`) | `entries`, `page-reload`, `tab-return`, `page-ends` |
| Экстеншен `background/handoff/hls/`, `range-fetcher.ts`, `file-*` | `capture --page m3u8-in-src --page hls-fmp4-abr --page hls-aes128 --page video-src-cdn --page video-src-referer`, `player-commands`, `hls-live`, `local-address` |
| Экстеншен `background/handoff/episode-change/`, `session-page-reload.ts` | `episodes`, `page-reload` |
| Экстеншен `background/handoff/page-return/`, `page-tab-reveal*` | `tab-return`, `page-return` |
| Экстеншен `background/auto-handoff/`, `page/auto-handoff/`, `content/auto-handoff-watch*` | `auto-handoff` |
| Экстеншен `adapters/` (сайты, руки, меню), `background/choice-memory/` | `series`, `series-memory`, `episodes`; `adapters/capabilities/ad-organs/` — `capture --page ad-skip` (все три варианта пропуска) |
| Экстеншен `background/tab-model/`, `local-address-block-rules.ts` | `local-address`, `attacks` |
| Экстеншен `background/entries/`, `popup/`, `content/page-ui/` | `entries`, `extension-status`, `page-return` |
| Экстеншен `background/codec-policy/`, `page/codec-policy/` | `codecs` |
| Экстеншен субтитры (`page/page-subtitles/`, `background/handoff/page-subtitles/`) | `page-subtitles` |
| Плеер `app-egui/src/browser_session/session_media/` (поколения, часы страницы, перемотка) | `capture`, `player-commands`, `episodes`, `page-ends` |
| Плеер `browser_session/window/` (кадр окна, концы, удержание кадра 380) | `page-ends`, `episodes`, `page-reload`, `quality` |
| Плеер `browser_session/hls_bridge/` | `capture --page m3u8-in-src --page hls-fmp4-abr`, `hls-live`, `live-ends`, `hls-live-sleep` |
| Плеер `browser_session/page_variants*`, `page_subtitles/` | `series`, `page-subtitles` |
| Плеер `browser_session/journal*` (строки журнала — глаза стенда) | `cargo test -p browser-bridge-bench`, `smoke` |
| `pushed-media-source` | `capture`, `player-commands`, `attacks` |
| `browser-bridge-protocol`, `browser-bridge-session` | `attacks`, `extension-status`, `page-ends` |
| `browser-bridge-host`, `browser-bridge-install` | `extension-status`, `smoke` |
| Ядро плеера, декодеры, рендер (`player-core`, `video-*`, `render-*`, `audio*`) | `capture --page mse-av1 --page mse-muxed` |
| Сам стенд (`crates/browser-bridge-bench`) | `cargo test -p browser-bridge-bench` и команды, чьи сценарии или судьи правились |

## Что доказывает каждая команда

| Команда | Что делает | Что доказывает |
|---|---|---|
| `pages` | Открывает каждую страницу стенда в чистом браузере (без экстеншенов) | Сами страницы честные: видео в них реально играет силами браузера, их собственные проверки проходят. Если здесь провал — дальше проверять мост бессмысленно |
| `smoke` | Страница `mse-fmp4` в режиме передачи + **настоящий** экстеншен + **настоящий** посредник + **настоящий** плеер | То же, что `capture`, но для одной страницы (самый быстрый ответ «мост вообще жив») |
| `capture` | Каждая MSE-страница (`mse-fmp4`, `mse-webm`, `mse-av1`, `mse-muxed`, `mse-abr`, `mse-stall-reload`, `mse-generation-swap` и его варианты, `cached-descriptors`, `shadow-dom`, `iframe-cross-origin`, `cdn-guard`, `video-src*`, родной HLS VOD `m3u8-in-src` / `hls-fmp4-abr` / `hls-aes128`, см. раздел 10a2) в режиме передачи: страница играет, **робот «отдаёт вкладку»**, плеер показывает кадры | Цепочка по звеньям: экстеншен принял `hand_off_tab` и вернул `started` → посредник выдал `session_granted` → плеер показал `presented_frames >= 50` → у страниц с действием под тенью (`mse-abr`: смена качества, `mse-generation-swap`: новый MediaSource) кадры плеера растут и после действия → страница под тенью жива (часы идут, буфер докачивается, сторож зависания не перезагрузил MediaSource лишний раз) → **браузер не декодирует**: настоящий счётчик кадров мимо тени почти не растёт. Первыми идут контрольные прогоны без экстеншена (`(control)`): браузер играет сам, линейка обязана насчитать обычное воспроизведение (иначе «браузер не декодирует» ничего не доказывает) |
| `page-ends` | Страницы, которые сами заканчивают передачу (`source-withdrawn`, `source-to-file`, `page-freeze`), через настоящий экстеншен и плеер | Плеер честно, вовремя и правильно показывает конец (решения 179/180): сообщение в журнале окна (`Browser session end notice shown notice=… cause=… end=…`) нужное и в срок, окно закрылось, доигрывания не было (`Playback drain finished` нет), страница отчиталась `pass` (подробности ниже) |
| `player-commands` | `mse-fmp4`, `video-src` и `hls-vod-long` (родной HLS, 40 с) под тенью; стенд управляет окном плеера по MPRIS на частной шине | Пауза плеера → страница на `paused`, часы стоят; play → часы идут; перемотка плеером → часы страницы у цели, страница докачала новое место, кадры плеера растут, позиция плеера идёт; закрытие окна плеера (`Quit`) → тень снята, браузер снова декодирует, страница сама играет с позиции закрытия |
| `extension-status` | **Настоящий** экстеншен из `extensions/browser/dist/<браузер>`; открывает его попап | Собранный экстеншен 07 видит плеер: попап пишет «Connected to Fastiplayer» |
| `replay` | страница `replay-recording` + запись потока страницы `*.fprec` (`--recording`, `--min-frames`) | Файл разобран (магия, длина, поля), страница повторяет события настоящими вызовами MSE в исходном темпе (главный поток; `worker-*` — в blob-воркере, только Chromium, в Firefox честное unsupported), в записанный `handoff_began` робот отдаёт вкладку плееру, плеер показывает ≥ `--min-frames` кадров; печатаются сводки плеера (`presented_frames`, `position_ms`, `state`) |
| `generate-recording` | синтетическая запись из fMP4-ассета стенда (`--output`, `--asset`) | Файл читается обратно разборщиком; даёт `replay` вход без записи с настоящего сайта |
| `codecs` | Страница `codec-choice` спрашивает кодеки AV1 → VP9 → H.264 при разных настройках экстеншена и метках плеера | Экстеншен отвечает за плеер по настройке и метке, HLS отвергается всегда, выбранный ролик играет |
| `attacks` | (09c) Вредные страницы `attack-giant-segments`, `attack-event-flood`, `attack-garbage`, `attack-worker-flood` (Chromium) под тенью, потом в ТОМ ЖЕ браузере чистая `mse-fmp4` | Атака кончается честно (сообщение плеера `stream_error`; для мусора ещё допустим отказ поколения `playback_failed`; шквал сообщений воркера обязан быть ПОГЛОЩЁН: конца нет, кадры идут, часы идут, провал главного потока в границе `landing.rs`; гигантские сегменты обязаны долететь: `giant_bytes_accepted` > 0), окно закрыто, страница-атака отчиталась `pass`; потом чистая страница: `started`, **новый** пропуск посредника, кадры нового окна, браузер её не декодирует, паники нет |
| `local-address` | (09c) Файл на `127.0.0.1` под страницей с «публичным» именем `public.bench.example`: настройка по умолчанию, `ask`, согласие на этот хост и на другой, согласие при `block`, публичный файл с 302 на `127.0.0.1` | Решение 199: `local_address_refused` / `local_address_needs_consent{host}` / `started` с кадрами; в отказах сеанса нет, фон файл не качал, браузер играет сам. Перенаправление: журнал сервера не видит фона на локальном файле, плеер честно отказывается от поколения. HLS (10a2, `hls-ts-long`): плейлист и первые 3 сегмента публичные, остальные — `http://127.0.0.1:<порт>/media-local-target/…`; `started`, фон взял плейлист и публичные сегменты, но ни одного локального, кадры публичной части допустимы, сеанс кончился честно |
| `hls-live` | (10a3) Страница `hls-live`: родной HLS LIVE (скользящее окно из 4 сегментов `hls-ts-live`, без `ENDLIST`) под тенью; закрытие окна плеера | Элемент под тенью не качает; фон перезапрашивает плейлист (≥ 3) и идёт вслед за эфиром (сегмент за начальным окном); кадры плеера растут 25 с; страница видит `duration = Infinity` и непустой `seekable`, чей конец растёт; после `Quit` элемент перезагрузил `src`, грузит свежее окно и играет |
| `hls-live-sleep` | (16d4, решения 333–334) Страница `hls-live-sleep`, только Chromium: родной HLS-эфир, сервер которого через 30 с после первого плейлиста 20 с отвечает на плейлист 503, а окно эфира едет дальше (после «пробуждения» в нём нет ни одного прежнего сегмента). Маршрут `/cdn/hls-live-sleep/` (`site/hls_live_sleep.rs`) | Годность: передача, первые кадры и удачный плейлист фона — до сна. Фон получил ≥ 2 ответов 503 за ≥ 10 с, после сна плейлист снова 200, фон качал сегменты нового окна; в журнале плеера «HLS live network recovered» и «rejoined near the live edge», после склейки ≥ 100 кадров, откатов позиции 0, отказов потока нет; страница под тенью жива, её часы идут. Судит `scenarios/hls_live_sleep/judge.rs` |
| `entries` | (10b) Матрица «вход × страница» на НАСТОЯЩЕЙ мыши и клавиатуре в невидимом kwin 1920x1200: клик по значку экстеншена на панели, Tab и Enter в попапе. Входы — перечисление `Entry` (сейчас `toolbar-button`; 10c добавит правый клик и ссылку). Строки: старт на `mse-fmp4`, `mse-webm`, `video-src`, `iframe-cross-origin`, `shadow-dom`; вход посреди проигрывания; выбор другого видео (`banner-with-main`); `m3u8-in-src` (родной HLS — обычный вход); отказы `no_video` / `protected` / `not_supported_yet` (`native-hls` сам отказывается до входа; `blob-file` — форма `blob_file`); вопрос про локальный адрес (Allow / Don't allow); страница браузера (`tab_not_scriptable`) | `hand_off_tab` стенд не зовёт. Кадры ≥ 50 в плеере, браузер под тенью не декодирует, попап закрылся сам; выбор баннера: файл баннера фон запросил только после выбора, два пропуска в одном процессе плеера, главное вернулось на страницу на паузе; отказы: текст `data-notice` в попапе, сеанса нет. Нужны `spectacle` и `ffmpeg` |
| `episodes` | (16a, решения 303–309) Пауза между сериями и уход одностраничника через настоящий экстеншен и плеер, 4 строки в обоих браузерах. Страницы `episode-gap`, `episode-spa-away`; серию 1 до конца доводит робот (MPRIS `Seek` за 8 с до конца). `delayed-next`: серия 2 приходит через 20 с (дольше прежних 5 с), плеер стартует с запомненным полным экраном (профиль `RememberedFullscreen`, `[browser_session.window] fullscreen = true`) | Окно создано один раз и процесс один; `fullscreen=true` и ни разу `false`; «ждём следующую серию» → снято `reason=generation_shown`; кадры растут после серии 2; смена заголовка с ревизией ≥ 1; сеанс не кончался. `delayed-next-hidden`: то же, вкладку страницы стенд спрятал (страница пишет `visibility=hidden`). `no-next-episode`: серии 2 нет — `notice=next_episode_not_started` через 55–75 с после снятия источника, окно закрылось, доигрывания нет. `spa-away`: страница сменила адрес (`pushState`), сняла источник — `cause=tab_gone` (`finished`/`waiting_for_viewer`), НЕ `page_took_video_back`. Прогон ~7 мин на браузер |
| `page-subtitles` | (13b2) Субтитры, которые страница берёт САМА: `subtitles-track` (`<video src>` + `<track default>` en и выключенная uk), `subtitles-fetch` (`.vtt` через `fetch` со второго origin), `subtitles-xhr` (`.srt` XHR текстом, затем json3 XHR как JSON), `subtitles-text-track` (как hls.js: `addTextTrack` + реплики, после передачи дорожка растёт). Файлы — `site/subtitles/` (реплики каждые 2 с до 300 с). `--page` сужает выбор | В журнале плеера `Page subtitle track ready`: ожидаемые дорожки (язык, формат, `selected`: en=true/uk=false; fetch=1 включена; xhr=srt выключена + json3 включена; text-track=en включена), у растущей дорожки `Page subtitle document received … replaced=true` и больше реплик; `subtitle_overlay_frames > 0` в сводке плеера (ждём до 45 с); пропуск посредника, страница `pass`, нет запретных строк. Строки журнала разбирает `observe/page_subtitle_log.rs`, судит `scenarios/page_subtitles/judge.rs` |
| `page-return` | (11a, решения 236–238) Заглушка «Playing in Fastiplayer [Return]» и честные концы на НАСТОЯЩЕЙ мыши в невидимом kwin. Строки: заглушка над видео (`return-kit`); «Return» на `return-kit`, `return-offset` (поток со 600-й секунды) и `return-baked-ad` (реклама вшита в тот же поток); закрытие окна; замена из второй вкладки; «Return» и закрытие разом (оба порядка, видео с родными кнопками: опоздавший клик обязан съесть щит заглушки, иначе родные кнопки сами запустили бы игру); после «Return» своя перемотка, смена качества и следующая серия; `hls-live` (только Chromium); сломанный плеер страницы (`endOfStream("decode")`, и то же со снятием источника); «Return» на вкладке входа по ссылке. Отладка одной строки: `BRIDGE_BENCH_RETURN_ROW=<часть подписи>` | Заглушка ±2 px над видео, глотает клик, едет за прокруткой, уходит с концом сеанса; после «Return» страница играет сама с N ± 1 с (часы без скачка, настоящие кадры растут), окно ушло за ≤ 6 с, в журнале плеера `ClosedByPage { code: Some(4010) }` и ни доигрывания, ни прощания, ни сообщения, нового сеанса нет; закрытие окна — пауза на своей секунде и прощание; замена — A на паузе на своей секунде, B кадрами в том же окне; гонка — один возврат, `paused` сменился не больше раза, опоздавший клик до видео не дошёл; эфир у края; поломка — доигрывание и `cause=page_player_failed` не раньше 4 с, окно закрылось; вкладка по ссылке жива 8 с после «Return». Факты пишет страница (`site/lib/return-checks.js`), судит `scenarios/page_return/judge.rs` |
| `blocker-matrix` | (09c) Страницы `mse-fmp4`, `mse-worker-classic`, `video-src`, `codec-choice` × (без блокировщика / блокировщик первым / наш экстеншен первым) с uBlock Origin Lite (Chromium) или uBlock Origin (Firefox). **Не входит в `all`** | Каждая клетка доходит до кадра в плеере по приговору `capture`. Блокировщики качает `scripts/browser-bridge-bench-fetch-blockers.sh` |
| `auto-handoff` | (12a) Режим 2 «автозапуск» на НАСТОЯЩЕЙ мыши в невидимом kwin: человек сам нажимает Play на видео со звуком, и экстеншен без кнопки «Отдать» уводит его в плеер. Настройка `autoHandoffSettings` пишется в `chrome.storage.local` экстеншена из его служебной страницы, затем страница открывается заново (охрана включается на старте документа); порог длины в стенде 45 с. Страницы ждут клика сами (автозапуск без клика в стенде разрешён: именно он доказывает, что экстеншен смотрит на жест). Строки: `default-off` (режим выключен, клик — передачи нет); `click-hands-off` (фильм 60 с со звуком); `muted-autoplay` (беззвучный баннер, заигравший сам); `hover-preview` (превью по наведению мыши); `short-clip` (30 с со звуком); `preroll-to-film` (реклама рядом с ждущим фильмом по клику: передача на рекламе и переезд на фильм); `site-rule-never` и `own-stop-list`; `after-return` («Return» настоящим кликом, затем окно 15 с без новой передачи, настоящая пауза и новый клик — передача снова); `three-tabs` (играет последняя, две первые на паузе на своих секундах); `live-hls` (только Chromium) и `unknown-duration` (`duration = Infinity`). Отладка одной строки: `BRIDGE_BENCH_AUTO_HANDOFF_ROW=<часть подписи>` | Строка «передача»: кадры в рендере плеера (`presented_frames`), заглушка с «Return», браузер под тенью почти не декодирует (судит тот же `browser_does_not_decode`). Строка «нет передачи»: ни одного пропуска посредника, заглушки нет И положительный факт — за окно 12 с страница играла силами браузера (настоящие кадры мимо тени растут, часы идут), настоящий клик с активацией пользователя дошёл до страницы. Факты пишет страница (`site/lib/auto-handoff-kit.js`), судит `scenarios/auto_handoff/judge.rs` |
| `page-reload` | (16c, решения 59, 304, 305) Страница с переданным видео перезагружается или уходит по адресу В ТОЙ ЖЕ вкладке; уводит её сама страница по команде робота `hook:navigate` (настоящая навигация сайта). 5 строк в обоих браузерах, каждая в свежем окружении. Страницы `page-reload`, `page-reload-frame` (плеер в iframe чужого origin). Замена: `reload-same-url`, `next-episode-new-url` (`ep=2` на том же origin), `iframe-player-reload` (вкладка на главном origin, плеер в рамке `localhost`); профиль `RememberedFullscreen`. Конец: `page-without-video`, `other-site` | Замена: окно и процесс по разу; ровно два сеанса, один `grant=ReplacedPrevious`; `fullscreen=true` и ни разу `false`; кадры растут (≥ 30) после замены; браузер второй документ не декодирует (потолок `capture`); страница ушла именно туда (вид и серия второго документа, origin вкладки). `page-without-video`: первый признак конца не раньше 9 с после ухода страницы (экстеншен ждёт 10 с), затем `notice=finished cause=tab_gone`, окно закрылось, замены нет. `other-site`: первый признак конца не позже 6 с, нет `ReplacedPrevious`, видео нового документа играет сам браузер (≥ 30 настоящих кадров). Прогон ~7 мин на браузер |
| `all` | `pages`, `capture`, `page-ends`, `player-commands`, `extension-status`, `codecs`, `attacks`, `local-address`, `hls-live`, `live-ends`, `hls-live-sleep`, `page-subtitles`, `episodes`, `entries`, `page-return`, `tab-return`, `page-reload`, `auto-handoff` по очереди (`smoke` входит в `capture`; `blocker-matrix` — отдельно); с `--page` — только шаги со страницами (`pages`, `capture`, `page-ends`, `attacks`) | — |

Браузеры по умолчанию: Chromium и Firefox (`--browser chromium|firefox|both`). Chrome из
Flatpak проверяется руками на вехах (`scripts/browser-bridge-live-check/run-chrome-flatpak.sh`).

### Одна страница вместо всех: `--page`

```bash
scripts/browser-bridge-bench.sh capture --page mse-generation-swap-delayed --browser chromium
scripts/browser-bridge-bench.sh page-ends --page page-freeze
scripts/browser-bridge-bench.sh pages --page source-withdrawn --page source-to-file
```

Флаг повторяется и работает у `pages`, `capture`, `page-ends` и `all` (в `all` остаются только
шаги со страницами). Неизвестная страница, страница не из этой команды или `--page` у команды
без страниц — ошибка использования (код 2), а не молчаливый пустой прогон.

### Родной HLS VOD (сессия 10a2)

Экстеншен передаёт плееру `m3u8` из `<video src>`: плейлисты, сегменты и ключ качает ФОН
экстеншена сетью браузера (куки, Referer), элемент под тенью ничего не качает. Живой HLS
(`EXT-X-ENDLIST` нет) не поддержан до 10a3. Chromium играет родной HLS сам; Firefox на Linux нет
(`unsupported` разрешён таблицей страниц).

Ролики (`media/generate.sh`, URI в плейлистах относительные, GOP и сегмент 2 с, выжженный номер кадра):

| Ролик | Раскладка | Длина |
|---|---|---|
| `hls-h264-aac` | MPEG-TS, одно качество (`hls/master.m3u8` → `stream.m3u8`) | 12 с |
| `hls-fmp4-abr` | fMP4 (`EXT-X-MAP`, `init-N.mp4`, `seg-N-NNNNN.m4s`), главный плейлист с двумя качествами 640x360 и 320x180 (оба со звуком) | 12 с |
| `hls-ts-aes128` | MPEG-TS, сегменты под `EXT-X-KEY:METHOD=AES-128` (`key.bin` рядом, ключ `HLS_AES_KEY_HEX` в скрипте) | 12 с |
| `hls-ts-long` | MPEG-TS, одно качество, 20 сегментов: запас на паузу и перемотку | 40 с |
| `hls-ts-live` | MPEG-TS 320x180, 300 сегментов: источник живых эфиров `/cdn/hls-live/`, `/cdn/hls-live-ends/`, `/cdn/hls-live-sleep/` (16d4: удлинён до 600 с под настоящий сон машины) | 600 с |

Страницы (все `unsupported` в Firefox): `m3u8-in-src` (TS), `hls-fmp4-abr`, `hls-aes128`,
`hls-vod-long` (для `player-commands`), `hls-local-segments` (для `local-address`). Общий код —
`site/lib/native-hls-page.js`. Пример отказа `not_supported_yet` теперь `blob-file` (`blob:`-файл в `src`).

`hls-live` (10a3): эфир строит маршрут `/cdn/hls-live/<прогон>/stream.m3u8` (`site/hls_live.rs`) из сегментов
`hls-ts-live` по часам стенда (отсчёт от первого запроса плейлиста прогона; 4 сегмента сразу, дальше по
сегменту за `TARGETDURATION`; без `ENDLIST`; после конца ролика (600 с, 300 сегментов; прогон короче), окно замирает, а не
зацикливается). Бегун — `src/scenarios/hls_live.rs`, судья — `hls_live/judge.rs`. Факт Chromium 156:
у нативного элемента на таком плейлисте `seekable` пуст, `duration = Infinity`, элемент после
перезагрузки `src` стартует с НАЧАЛА окна (`currentTime` ≈ 0).

Судья `capture` — `src/scenarios/capture/hls_evidence.rs`, по журналу сервера: до передачи `<video>`
страницы просил плейлист (`Sec-Fetch-Dest: video`); после (+1,5 с на запрос «в полёте») элемент не
просит НИЧЕГО из каталога ролика; фон (скриптовые запросы) скачал ≥ 1 плейлист и ≥ 1 сегмент, у
`hls-aes128` ещё и `key.bin`; плейлисты и ключ — `200`, сегменты — `200` или `206`.

`player-commands` на `hls-vod-long` идёт на профиле плеера `SmallFileWindow` (малая предзагрузка) и
после общих пауза/play делает шаг `seek-beyond-fetched-reaches-network` (`player_commands/hls_steps.rs`):
цель — самый дальний сегмент (с запасом 12 с до конца), которого фон ещё не запрашивал; после
перемотки фон обязан запросить НОВЫЙ сегмент у цели (±4 с). Если плеер уже скачал всё впрок —
честный провал «нечего перематывать». Возврат: после закрытия окна плеера элемент снова просит плейлист.

`local-address`, случай `hls-local-segments-are-not-fetched` (`capture/local_address_hls.rs`): страница
под `public.bench.example`, корневой плейлист отдаёт маршрут стенда `/cdn/hls-local-segments/<путь>`
(`site/hls_local_segments.rs`) и на лету оставляет первые 3 сегмента публичными (`/media/…`), а остальные делает абсолютными `http://127.0.0.1:<порт>/media-local-target/…` (те же файлы под другим префиксом, чтобы журнал различал запросы; Chromium под публичным именем не играет HLS, где ВСЕ сегменты локальные).
Ждём: `started`, фон взял публичный плейлист и публичные сегменты, ни одного скриптового запроса к `/media-local-target/…`, кадры публичной части допустимы, честный конец сеанса (любая причина; сейчас плеер называет её
`unparsable_media`, backlog 39).

### Страницы сессии 09a2 (переходы источника и честные концы)

Варианты смены поколения (`capture`, хук `swap-generation`, проверки как у `mse-generation-swap`):

| Страница | Что делает страница | Что ещё проверяет |
|---|---|---|
| `mse-generation-swap-srcobject-null` | перед новым MediaSource `srcObject = null` (так делает YouTube перед рекламой) | то же, что у `mse-generation-swap`: второй MediaSource на том же элементе, часы нового ролика идут, кадры плеера растут |
| `mse-generation-swap-reload` | перед новым MediaSource `removeAttribute('src'); load()` | то же |
| `mse-generation-swap-delayed` | данные нового поколения приходят через 5 с (движок не делает ни одного запроса) | `clock-stays-at-new-start-while-waiting` (часы страницы ≤ 1 с, пока данных нет), `no-ended-while-waiting` (нет `ended`), `waiting-window-was-observed` (набрано ≥ 8 проб, иначе проверка пуста); потом обычный рост кадров плеера |
| `cached-descriptors` | обычный скрипт в `<head>` до передачи кэширует геттер `currentTime` и `play`; хук 5 с зовёт кэш | кэшированный геттер отдаёт время тени (расхождение с `video.currentTime` < 1 с и часы идут), а настоящий счётчик кадров мимо тени под тенью ≈ 0 (общий приговор `capture`) |

Сценарий `page-ends` (страница сама заканчивает передачу; срок считается от команды роботу):

| Страница | Действие по команде | Плеер обязан |
|---|---|---|
| `source-withdrawn` | остановить свой движок, `srcObject = null; removeAttribute('src'); load()` и больше ничего | через 4–12 с (допуск тени на пустой источник — 5 с): `notice=page_took_video_back cause=page_took_video_back end=none` |
| `source-to-file` | поставить обычный mp4 (`progressive-h264-aac`) и `play()` | быстрее допуска (≤ 4,5 с): то же сообщение; страница сама играет файл — настоящий счётчик кадров растёт (`page-plays-the-file-by-itself`) |
| `page-freeze` | заблокировать главный поток на 15 с | через 8–15 с (предел тишины пульса — 10 с): `notice=stream_error cause=page_stream_error end=none`; после разморозки страница снова владеет видео (`page-gets-the-video-back-after-freeze`: настоящий счётчик растёт) |

Для всех трёх приговор `page-ends` ещё требует: окно плеера закрылось (имя MPRIS ушло с шины),
строки `Playback drain finished` в журнале нет, посредник принял вызов и выдал пропуск, страница
подтвердила действие и отчиталась `pass`. Итоговые проверки «часы идут под тенью» этим
страницам не нужны (источник снят / заменён / главный поток стоит), они отключаются поимённо
опцией `skipFinishChecks` страницы (`site/lib/handoff-mode.js`), метрики остаются. В режиме
`pages` (чистый браузер, без передачи) эти страницы проверяют только то, что умеют сами:
пустой элемент после снятия источника, игру файла, живую страницу после заморозки, согласие
кэшированного геттера с `currentTime`.

Опции мини-плеера для своих страниц (`site/lib/bench-player.js`):
`newGeneration({ releaseOldSource: 'src' | 'srcObject-null' | 'remove-attr-load',
firstRequestDelayMs })`; `autoplay` вычисляется до снятия старого источника.

### Страницы сессии 09c (модель вкладки, DRM, реклама, атаки)

`capture` (хук — действие страницы по команде робота):

| Страница | Что ждём |
|---|---|
| `drm-probe` | Опрос ключевых систем DRM (`requestMediaKeySystemAccess`) защитой НЕ считается: видео передаётся и доходит до кадра |
| `series` (хук `next-episode`), `spa-navigation` (хук `navigate-spa`) | Новое поколение под тенью (следующая серия, переход `pushState`) доходит до кадра: кадры плеера растут и после хука |
| `banner-with-main` (хук `resume-main`, посылается ДО ожидания кадров) | Главное видео (60-секундный файл) стоит на паузе, рядом играют беззвучный зацикленный баннер и превью по наведению. Главное уходит в плеер (кадры + браузер его не декодирует), а баннер и превью остаются у браузера: страница сама пишет метрики `banner_real_frames`, `preview_real_frames`, `main_real_frames_after_resume`, а сервер подтверждает, что файл приманки фон не качал |
| `video-src-referer` (хук `own-fetch-without-referer`) | Файл на `/cdn/media/` (кука И свой Referer). Запросы фона несут Referer страницы и куку (журнал сервера); запросы самого `<video>` не изменились; хук — проба утечки правила: страница просит тот же адрес с `referrerPolicy: no-referrer` (честно 403) и с обычным Referer (206), запросы помечены `x-bench-page-fetch` |

Отказы (`capture`, строка `refusals`): `drm-clearkey` → `protected`, `banner-autoplay` (только баннер и превью) → `no_video`; в обоих сеанса нет, браузер продолжает декодировать сам. Исхода `ad_playing` больше нет (09c2): строка `ad_playing` от фона для стенда — неизвестный исход и ошибка разбора.

Реклама → фильм (09c2, решения 201 и 204; `capture`, модуль `scenarios/capture/ad_hop`, общее тело страниц `site/lib/ad-hop-kit.js`). Рекламные ролики БЕЗ цикла (`ad.mp4` 10 с, `ad2.mp4` 8 с), робот отдаёт вкладку посреди рекламы (метка страницы `handoff-ready`), реклама кончается или её обрывают, страница запускает фильм (метка `film-started`). Плеер обязан доиграть рекламу и показать фильм В ТОМ ЖЕ окне:

| Страница (шаг) | Форма стыка |
|---|---|
| `ad-preroll`, `ad-preroll-unlisted` (Tortuga: домен не из списка, пустой `<video>` рядом) | отдельный элемент рекламы; по `ended` `src=""; load(); remove()` и в той же задаче новый элемент фильма и `play()` |
| `ad-beside-film` (Ashdi) | фильм MSE уже загружен на паузе, реклама поверх; по `ended` реклама убрана и `film.play()` |
| `ad-film-frames` | реклама в iframe другого origin (`ad-frame-inner`), фильм в родительском документе; по `ended` iframe удаляется |
| `ad-film-same-element`, `…-file` (`&film=file`) | реклама и фильм в одном `<video>`: новый blob MediaSource или обычный файл (60 с) |
| `ad-chain` | три ролика: тот же элемент через 250 мс (`setAttribute('src')`), потом новый элемент после 1,2 с с `play()` до `src`, потом фильм |
| `ad-skipped-midway` (хук `skip-ad`) | реклама оборвана посреди ролика, `ended` нет; робот шлёт хук, когда плеер уже на рекламе |
| `ad-skip`, `&ads=2` | 09d: органы в форме PlayerJS; плашка «Ad N of 2», перемотка запрещена, робот жмёт `Next` (MPRIS), нажата настоящая кнопка страницы, фильм в том же окне |
| `ad-skip`, `&skip=never` | 09d: непропускаемая реклама: счётчик есть, кнопки нет ни разу |
| `ad-skip`, `&skip=trusted` | 09d: кнопка отвергает клик из кода, реклама доигрывает, сеанс цел |
| `ad-then-banner` | после рекламы заиграл беззвучный `loop`-баннер, фильм через 2 с: фильм в плеере, баннер у браузера |

Приговор (`judge_ad_hop`, чистый, с юнит-тестами на каждое нарушение): `hand_off_tab` → `started`; ровно один `session_granted` за прогон и ни одной строки `Browser session end notice shown`; в сводках плеера `loads=` не меньше 2 (цепочка — 4); после `film-started` показ кадров вырос минимум на 60 между двумя сводками; страница сообщает метрики: часы фильма за ~10 с ушли минимум на 1 с, настоящий счётчик кадров фильма не больше 15 (рекламы — не выше обычного потолка), фильм не на паузе, `play()` фильма исполнено (не `AbortError`), `ended` сработал ровно один раз на каждый ролик; журнал сервера: фон рекламный файл качал (для `ad-then-banner` файл баннера фон не качал, а сам баннер декодируется, не меньше 30 кадров).

`ad-no-film` идёт сценарием `page-ends`: реклама кончилась, страница убрала элемент и больше ничего не ставит; через ~15 с (окно 14-18 с от метки страницы `ad-removed`) плеер пишет «The page took the video back» (`notice=page_took_video_back cause=page_took_video_back end=none`), `Playback drain finished` нет.

Атаки (`attacks`): общее тело — `site/lib/attack-page.js` (честный старт, хук `attack`, итоговые проверки живой тени отключены поимённо, страница сама не бросает ошибок). Размер гигантских сегментов (`GIANT_SEGMENT_BYTES`) заведомо больше лимитов решения 200: если лимиты поднимут, поднять и константу. Атака на воркер сыплет `noise` в обе стороны и пишет метрики стоимости (`flood_messages_per_s`, `flood_main_thread_worst_gap_ms`).

Защита локального адреса (`local-address`): страница `video-src-local` открывается под именем `public.bench.example`. Имя браузер разрешает в `127.0.0.1`: Chromium — `--host-resolver-rules="MAP … 127.0.0.1"`, Firefox — pref `network.dns.localDomains` (оба способа проверены на пустом сервере). Настройка пишется в `storage.local` под ключом `localAddressSetting` (`block|ask`, константа `LOCAL_ADDRESS_SETTING_KEY` в `capture/local_address.rs` — сверять с экстеншеном), согласие человека — поле `consented_local_host` (хост, про который спрашивали) в сообщении робота. Случай с перенаправлением берёт файл с `file_route=redirect-local` (сервер отвечает 302 с `/cdn/redirect-local/…` на `/media/…` под `127.0.0.1`) и идёт на профиле `ShortNetworkRetry`: бюджет повторов плеера 3 с вместо минуты.

Матрица блокировщиков (`blocker-matrix`):

```bash
scripts/browser-bridge-bench-fetch-blockers.sh      # один раз: качает и сверяет SHA-256
scripts/browser-bridge-bench.sh blocker-matrix --browser chromium
```

Версии и хеши прибиты в скрипте (официальные релизы GitHub: `uBlockOrigin/uBOL-home` для Chromium, `gorhill/uBlock` для Firefox); файлы лежат в `target/browser-bridge-bench/blockers/<имя>/` и в репозиторий не попадают. Нет файлов — шаг падает с этой командой. Порядок установки задаёт `RunTweaks` (`src/scenarios/run_tweaks.rs`): `ExtrasFirst` ставит блокировщик первым. Стенд НЕ доказывает, что блокировщик реально блокирует рекламу, — только что передача с ним доходит до кадра.

### Лента коротких видео (16b2, решения 354–360)

Окно плеера идёт за лентой: человек отдал плееру один ролик и листает ленту колесом мыши. Ролик ушёл с экрана, страница сама его остановила (или убрала), и окно переезжает в ТОМ ЖЕ окне (один сеанс) на ролик, который страница запустит следующим. `capture --page feed` и `capture --page feed-hover` (модуль `scenarios/capture/feed`, судья `feed/judge.rs`, общие куски страниц `site/lib/feed-kit.js` и `feed-shapes.js`). Ролики — ассеты `reel-1..3` (30 / 42 / 36 с, кадр 640x360 / 512x288 / 384x216, выжженные `REEL N`, звук разных тонов): окно видно по строке журнала плеера `Output surface pool reconfigured for stream width=…`. Колесо — настоящее: `desktop_input::Mouse::scroll_at` (запрос `axis` протокола `fake_input`) на невидимом экране мышиного стенда; вкладка ленты при передаче остаётся видимой (`hand_off_from_service_page`), как у человека.

| Строка (шаг) | Что делает страница | Чего ждём |
|---|---|---|
| `feed-many` (Instagram) | свой `<video>` и MediaSource на ролик; ушедший с экрана целиком ставится на паузу, `history.pushState`, через 80-400 мс `play()` следующего (без звука); старый элемент убирается через ~6 с | две прокрутки: поколения `1, 2, 3`, ширины кадра `640, 512, 384`, один сеанс, кадры растут после каждого переезда |
| `feed-moved` (YouTube Shorts) | один элемент переносится в секцию следующего ролика (убрать и вставить одной задачей), новый MediaSource, паузы нет | одна прокрутка: новое поколение в том же окне (`1, 2`, ширины `640, 512`) |
| `feed-pool` (TikTok) | два элемента по очереди; прежний убирается из документа и ставится на паузу, заранее загруженный показывается и играет со звуком; адрес тот же | как `feed-many` |
| `feed-hover-preview` | главный ролик на экране и играет (ролик `reel-6`, 90 с, ширина 640: наблюдение начинается посреди ролика и не должно застать его конец, решение 385); рядом превью (без звука), которое запускается, когда настоящая мышь пришла на него | раздражитель был (`preview-start-trigger why=hover`, `preview-playing`), поколение одно, ширина одна (`640`), кадры растут |
| `feed-scroll-to-comments` | колесом главный ролик уходит с экрана целиком, страница его НЕ останавливает; ниже заиграло превью | раздражитель был (`main-left-screen paused=false`, `preview-playing`), поколение одно, кадры растут |

Приговор (`judge_feed`, чистый, юнит-тест на каждое нарушение): `started`; ровно один `session_granted`; ни одного сообщения о конце сеанса; колесо ДОШЛО до страницы (`wheel trusted=true` — отдельная причина провала); страница сама запустила следующий ролик; номера поколений идут подряд без лишних и пропущенных; ширины кадра по порядку — нужные ролики; кадры плеера растут после каждого переезда. У отрицательных строк раздражитель обязан быть записан страницей: без этого строка прошла бы вхолостую.

Правило «первый кадр нового ролика без пустого кадра» (16b3c, решение 380; `feed/switch_judge.rs`, разбор `observe/media_switch_log.rs`) действует во всех строках лент (`feed`, `feed-edges`, прокрутка назад, «Назад» в истории), где окно меняет ролик, кроме отрицательных «окно осталось на первом ролике». Судится строка журнала плеера `Browser window media switch switch=<n> end=… outcome=… release=… empty_frames=<n> held_frames=<n> first_frame_ms=<n|none> render_generation=<n|none>`. Для каждой смены, кончившейся первым кадром нового ролика (`end=first_frame`, не первая строка окна `outcome=no_previous_frame`): `empty_frames == 0` и `first_frame_ms` не больше 2000 мс (`MAX_FIRST_FRAME_AFTER_SWITCH_MS`: «сразу после переезда»). Таких смен должно быть не меньше, чем переездов (загруженных поколений без первого); нет строк (старый плеер) или смена кончилась иначе (`load_failed`, `player_stopped`…) — провал «строки замера нет». Незнакомое значение `end`/`outcome`/`release` — провал разбора, а не пропуск.

Одна строка: `BRIDGE_BENCH_FEED_ROW=pool scripts/browser-bridge-bench.sh capture --browser chromium --page feed`. Старая сборка экстеншена (проверить, что строка отличает новое от старого): `FASTIPLAYER_BENCH_EXTENSION_DIST=<каталог с папками chrome и firefox>` подменяет `extensions/browser/dist` для всех сценариев с настоящим экстеншеном.

#### Края ленты (сессия 16b3, `capture --page feed-edges`)

Что настоящие сайты делают помимо спокойного листания (наблюдение без моста, Firefox 160, 19 ноября 2026). Страница `site/pages/feed-edges.html` (лента без серийной очереди `site/lib/feed-edge-feed.js`, формы `feed-edge-shapes.js`, превью другого раздела `feed-edge-previews.js`; ролики `reel-1..5`, ширины кадра 640 / 512 / 384 / 320 / 448), звенья робота `capture/feed/edge_steps.rs`, приговор `edge_judge.rs`. Быстрая серия крутит колесо подряд (`Mouse::scroll_in_place`, без пауз подвода), а не «рывок раз в 0,5 с».

| Строка | Что делает страница | Приговор |
|---|---|---|
| `feed-burst-many` / `-pool` / `-moved` | после спокойной прокрутки три жеста колеса подряд (страница листает жестами, `paging=wheel`: в Firefox родная прокрутка с прилипанием гасит быстрые щелчки), промежуток между шагами ~150-170 мс; промежуточные ролики получают `play()` и вскоре `pause()` (Instagram I4, TikTok T3, Shorts S3: по два MediaSource на шаг) | окно кончает на ролике 5 (ширина 448), ширины только вперёд, поколения строго растут (минимум 3; дыры в номерах не нарушение: у Shorts два MediaSource на шаг), кадры растут, сообщения о конце нет; страница доказала серию (`final_reel=5`, `burst_intermediates>=2`) |
| `feed-removed-mid-move-many` / `-pool` | прежний элемент удалён через 30 мс после `play()` следующего, посреди переезда | сеанс жив, окно на ролике 2 |
| `feed-removed-after-move-many` / `-pool` | через 0,3 с после переезда страница удаляет элемент, на который переехало окно (адрес тот же), потом играет ролик 3 | сеанс жив, окно идёт к ролику 3 |
| `feed-leave-section-many` (I6) / `-pool` (T5) / `-moved` (S4) | уход в другой раздел сценарием формы (адрес, удаление/перестановка/снятие источника, пауза уже удалённого) и играющее тихое видимое превью | честный конец `notice=waiting_for_viewer cause=tab_gone` (окно стоит на паузе, решение 370), превью НЕ принято (после начала ухода ни одного нового поколения, ширины `640, 512`). Shorts (S4, 16b3c, решение 381) — лента по списку адресов: сообщение о конце не позже 3 с (`SHORTS_LEAVE_END_DEADLINE`) после метки `leave-url-pushed` (часы робота), а не после снятого источника и 5 с тени |
| `feed-leave-preview-same-element-moved` (16b3c) | Shorts уходит на главную (`leave-url-pushed path=/`), через 0,4 с — превью новым MediaSource на ТОМ ЖЕ элементе (хук `leave-with-same-element-preview`, внутри отсрочки фона 1 с) | как S4: конец на паузе в пределах 3 с, поколение превью не принято |
| `feed-leave-section-serial` (16b3c, решение 374) | форма `moved` с `address=serial`: шаг — новое поколение и адрес `/serial/123/<n>` (сериал, не из списка лент), затем уход S4 | прежний конец 306: `notice=finished cause=tab_gone`, окно доиграло и закрылось — сериал лентой не стал |
| `feed-shorts-reel-dropped-waits` (16b3c, решение 382) | Shorts после шага на ролик 2 снимает источник ролика (`emptied`), адрес тот же (хук `drop-current-source`) | 15 с ожидания: ни сообщения о конце (ни 4009, ни «Source tab closed»), ни нового поколения, окно отвечает |
| `feed-leave-while-waiting` | конец ленты (окно ждёт), через 3 с адрес меняется на другой раздел и там играет тихое превью | то же: честный конец, поколения `1, 2, 3` |
| `feed-end-waits` | конец ленты без ухода (решение 356) | 15 с ожидания: ни сообщения о конце, ни нового поколения, окно отвечает |

Раздражитель каждой строки страница пишет в журнал, судья его требует (без него строка прошла бы вхолостую).

Одна строка: `BRIDGE_BENCH_FEED_ROW=feed-leave-section-moved scripts/browser-bridge-bench.sh capture --browser both --page feed-edges` (подстрока `step_id`; имя ни одной строки краёв не входит в имя другой — это держит юнит-тест). Строки Shorts 16b3c: `feed-leave-section-moved`, `feed-leave-preview-same-element-moved`, `feed-leave-section-serial`, `feed-shorts-reel-dropped-waits`.

#### Назад по ленте и «Назад» в истории (сессия 16b3b)

Наблюдение живых лент: назад почти всегда новый элемент с новым MediaSource и с нуля; если элемент Instagram ещё жив (назад сразу после вперёд), он продолжает с секунды, где встал на паузу. Страница `feed-edges`, параметр адреса `back=live|recreated|history` (формы — `site/lib/feed-edge-back.js`), звенья робота `capture/feed/back_steps.rs` и `history_back.rs`, приговоры `back_judge.rs` и `history_back_judge.rs`. Колесо ВВЕРХ — `WheelDirection::Up`. Окно ждёт строку журнала плеера `Browser stream generation shown generation=N start_ms=X`: по `start_ms` видно, с какой секунды ролик в окне.

Раздражитель этих строк — «страница ПОЗВАЛА `play()` при возврате» (`reel-back-play-called`), а не «ролик заиграл» (`reel-back-started`): пока окно держит часы элемента, `play()` его не оживляет, и «заиграл» — уже исход. `play()` с `AbortError` (`play-rejected …: AbortError` в журнале страницы) валит любую строку возврата: страница не должна получать ошибку, которой сама не вызывала (решение 371).

| Строка | Что делает страница | Приговор |
|---|---|---|
| `feed-back-live-many` | колесо вниз на ролик 2, вверх: ролик 2 на паузу, адрес ролика 1, `play()` ТОГО ЖЕ элемента | поколения `1, 2, 3`, ширины `640, 512, 640`, поколение 3 идёт с секунды паузы страницы (в пределах 1,5 с, заметно больше 0), кадры растут, конца нет |
| `feed-back-recreated-many` | элемент ролика 1 удалён, новый `<video>` с новым MediaSource играет ролик 1 с 0 | то же, но `start_ms` поколения 3 меньше секунды |
| `feed-back-recreated-pool` | TikTok: заготовленный элемент пула получает новый MediaSource ролика 1 | как `feed-back-recreated-many` |
| `feed-back-live-many-pressure` (замер, решение 369) | то же, но после переезда на ролик 2 страница качает дальние ролики сверх потолка памяти моста (64 МиБ, `ReelCrowd`); ролик 1 остаётся живым | в отчёт идёт `back_start_ms`, `back_waited_ms`, исход `immediate / waited / played_in_browser / never`; проваливает только `never` и общие провалы |
| `feed-back-live-many-far` (замер, решение 378) | вперёд на ролик 3, то же давление, затем колесом вверх к ролику 1 | проходят `immediate` и `played_in_browser` (окно ролик не взяло, сеанс жив, окно отвечает, страница играет ролик сама); `waited` без предела, `never`, конец сеанса проваливают |
| `feed-history-back-pool` (решения 375-376) | TikTok: уход в `/explore` сценарием T5, честный конец (окно стоит на паузе), через 2 с «Назад» в истории: страница возвращает в документ тот же элемент ролика 2 (тот же MediaSource), ставит на 0, играет | окно создано один раз, сеансов два (второй `grant=Started`), до подхвата поколения `1, 2`, после подхвата одно показанное поколение шириной 512 с `start_ms` меньше секунды, кадры растут, превью не принято |

Прогон одной строки: `BRIDGE_BENCH_FEED_ROW=feed-back scripts/browser-bridge-bench.sh capture --browser both --page feed-edges` (подстрока `step_id`; `feed-history-back` — строка истории).

### Живая мышь (сессия 10b)

Попап экстеншена открывается только настоящим кликом человека по значку на панели, поэтому стенд умеет то же самое:

- `desktop_input` — клиент протокола KDE `org_kde_kwin_fake_input` (перемещение, клик, колесо `scroll_at`, клавиша; `Key::Tab`, `Key::Enter`). Подключается по пути сокета невидимого экрана, после каждого запроса делает `roundtrip`, ошибки протокола возвращает.
- `screen_capture` — снимок экрана (`spectacle` внутри частного сеанса), разбор кадра через `ffmpeg`, поиск значка экстеншена в верхней полосе экрана по шаблонам 16–32 px из `extensions/browser/dist/<браузер>/icons/icon-128.png` (сравниваются только непрозрачные пиксели). Не нашёл или нашёл два места — ошибка с сохранённым снимком в каталоге прогона.
- `compositor_windows` — окна невидимого kwin через скрипт `org.kde.kwin.Scripting` на шине KWin (она отдельная от частной шины плеера): `bring_browser_forward` возвращает браузер вперёд, когда окно плеера забрало фокус.
- Значок на панели: Firefox — `action.default_area: "navbar"` в манифесте; Chromium — `extensions.pinned_extensions` в `Default/Preferences` профиля стенда (кладётся до запуска).
- `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1` ставится только нашему kwin (чистое окружение, только `DisplayTarget::Virtual` и `DesktopInputPolicy::Allowed`): без неё kwin не отдаёт `fake_input` и снимок экрана чужим клиентам. На `Real` такое окружение не создаётся вообще (ошибка), экран владельца синтетическим вводом не трогается.
- Размер экрана — настройка окружения (`DesktopSetup`): по умолчанию 1280x720 как раньше, мышиные сценарии берут 1920x1200 (на 1280x720 родное меню Chromium не влезает).

Живая проверка модулей без сценария: `cargo test -p browser-bridge-bench --test live_mouse -- --ignored --nocapture --test-threads=1` (нужны `cargo build -p app-egui -p browser-bridge-host` и собранный экстеншен). Тест кликает по значку, проверяет попап независимой уликой (цель DevTools у Chromium, chrome-контекст Marionette у Firefox), закрывает его Escape, проверяет Tab/Enter на тестовой странице и возвращение браузера вперёд после окна плеера. Снимки копируются в каталог из `BRIDGE_BENCH_SHOTS_DIR`.

### Как стенд «отдаёт вкладку» и почему страница не командует сама

Передачу запускает не страница, а **служебная страница экстеншена** (решение 175): та же
внутренняя команда `hand_off_tab`, что в продукте шлёт кнопка попапа. Робот стенда
(`BrowserUnderTest::hand_off_tab`) открывает `options.html` экстеншена, находит вкладку стенда
через `tabs.query`, шлёт фону `{kind:"hand_off_tab", tab_id}` и разбирает исход строго по
договору (`started` / `no_video` / `player_unavailable` / `failed`). Chromium — CDP
`Runtime.evaluate` с `awaitPromise`, Firefox — Marionette `ExecuteAsyncScript` (API экстеншена
в такой странице лежит на `window.wrappedJSObject`). Никаких тестовых дыр в продукте: ни
параметров адреса, ни тестовых сборок. Если у экстеншена нет обработчика команды, робот
получает `Receiving end does not exist`, и сценарий падает с этим текстом.

### Режим передачи страниц стенда (`mode=handoff`)

Это параметр адреса **сайта стенда**, а не продукта. Страница играет как обычно до своей
проверки «играет», пишет в журнал стенда `handoff-ready` и ждёт команд робота (опрос
`GET /bench/commands/<run>/<сколько получено>`; каждую команду подтверждает строкой
`ack <команда>`). Под «тенью» браузер не декодирует, поэтому страница отчитывается фактами,
которые видит сама (часы, `paused`, буфер наперёд, число MediaSource), и **настоящим
счётчиком декодированных кадров мимо тени**: тень подделывает `getVideoPlaybackQuality` на
прототипе своего окна, а у чистого `about:blank`-iframe, созданного страницей при загрузке,
свой прототип со своим методом (`createRealFrameProbe` в `site/lib/handoff-mode.js`; в обоих
браузерах проверено живьём: контрольный прогон насчитывает ~200 кадров за 8 с обычной игры).

Порог «не декодирует» (`capture.rs`): не больше 5 кадров в секунду плюс 10 кадров запаса на
момент перехода и окно не короче 6 с. Основание: в 00c браузер под тенью декодировал 0–116
кадров за 60 с (до ~2 к/с), ролики стенда играют 25 к/с.

## Изоляция: что стенд не трогает

- **Профили владельца не трогаются.** Каждый прогон живёт в своём каталоге
  `target/browser-bridge-bench/work/run-<pid>-<время>`: временный `HOME`, свои `XDG_*`, свой
  профиль браузера. Настоящие `~/.mozilla` и `~/.config/chromium` не читаются и не пишутся.
- **Своя шина D-Bus.** Плеер будится посредником через частную шину, а не через шину сеанса.
  Ваш настоящий плеер и его окно остаются в покое.
- **Невидимый экран.** По умолчанию `kwin_wayland --virtual` (1280x720), на вашем мониторе
  ничего не появляется.
- **Установка посредника — настоящим установщиком**, как кнопка «Connect» в плеере, но во
  временный HOME.
- После прогона всё убирается. При провале каталог прогона **сохраняется** для разбора, путь
  печатается в таблице; `--keep-work` сохраняет его и при успехе. Старые каталоги
  `target/browser-bridge-bench/work/` можно удалять руками.
- **Подробный журнал плеера для разбора сбоя:** `FASTIPLAYER_BENCH_PLAYER_LOG=info,video_vaapi=debug`
  (синтаксис фильтра `RUST_LOG`). Судьи читают строки `info`, поэтому фильтр должен их оставлять.
- **Падение плеера** ищется в системных дампах: `coredumpctl list`, разбор — `coredumpctl debug <PID>`
  (подключаться отладчиком к живому плееру стенда система не даёт: `ptrace_scope=1`).

## Посмотреть глазами

```bash
scripts/browser-bridge-bench.sh smoke --display real
```

Браузер и плеер рисуются на вашем настоящем экране (нужен `WAYLAND_DISPLAY`). Шина и профили
всё равно частные. Звук плеера в стенде всегда тихий (`FASTIPLAYER_TEST_BROWSER_AUDIO=simulated`).

Настройки плеера внутри стенда: `--player-config clean` (файла настроек нет, умолчания) или
`gles2-vaapi` (рендер OpenGL ES и аппаратное декодирование, как у владельца; файл пишется
боевым писателем настроек, поэтому не отстаёт от схемы).

## Как добавить страницу-сценарий

1. Положите страницу в `site/pages/<id>.html`. Она берёт общий мини-плеер из `site/lib/` и в
   конце шлёт отчёт (`pass` / `fail` / `unsupported`) на `report_origin` из своего адреса.
2. Добавьте строку в таблицу `PAGE_SCENARIOS` в `src/scenarios/page_catalog.rs`. Если страница
   вправе честно ответить `unsupported` в каком-то браузере, укажите его там же.
3. Запустите `scripts/browser-bridge-bench.sh pages`. Тест `table_matches_the_page_files_on_disk`
   следит, чтобы таблица и файлы не разошлись (вспомогательные страницы вроде `iframe-inner`
   перечислены в тесте отдельно).

## Дополнительный экстеншен (механизм для блокировщика рекламы)

```bash
scripts/browser-bridge-bench.sh smoke --extra-extension /путь/к/распакованному/каталогу
scripts/browser-bridge-bench.sh pages --browser firefox --extra-extension /путь/к/ublock.xpi
```

Флаг можно повторять. Дополнительный экстеншен ставится в КАЖДЫЙ запуск браузера рядом с
настоящим экстеншеном (в сценариях передачи), после него. Порядок «блокировщик первым» и готовый
набор страниц даёт `blocker-matrix` (он берёт блокировщики из `target/browser-bridge-bench/blockers/`
и игнорирует этот флаг). Chromium принимает только распакованные каталоги, Firefox — и каталоги, и
`.xpi`/`.zip` (временная установка). Матрица «какие блокировщики и страницы проверять» — `blocker-matrix`.

## Известные ограничения

- Каждая строка идёт в свежем окружении (свой экран, шина, плеер, браузер): около 10–25 секунд
  на строку, зато журналы и счётчики не перемешиваются между строками.
- Нижний предел строки задают окна судей, а не сводки: тень держится ≥ 6 с, фильм после
  рекламы ≥ 8 с, эфир HLS ≥ 15 с, серия в `episodes` ждёт следующую в реальном времени.
- Плеер стенда — release: отладочные проверки (`debug_assert`) в нём не срабатывают. Их
  ловят юнит- и процессные тесты `cargo test`.
- «Кадр с T» после перемотки проверяется ростом кадров и позицией плеера, а не выжженным
  номером кадра (фикстуры с выжженным номером — отдельная работа 12b).
- Команды плеера идут по MPRIS (`busctl` на частной шине); внутрь плеера стенд не лезет.

- Только локально: нужны Chromium, Firefox, `kwin_wayland`, `ffmpeg`, `npm`; в CI стенд не
  запускается.
- Первый запуск генерирует ролики через ffmpeg (кэш в `target/browser-bridge-bench/media`,
  ключ — отпечаток `media/generate.sh`), он дольше последующих.
- Из журнала плеера игнорируется одна известная безвредная ошибка: на частной шине нет
  портала настроек рабочего стола (`XDG Settings Portal did not return response in time`).
  Любая другая `ERROR` или `panic` — провал.
- `smoke` проверяет «кадры дошли до показа», а не качество картинки и не синхронность звука.
- Живые тесты: `cargo test -p browser-bridge-bench --test live_smoke -- --ignored --nocapture`.
