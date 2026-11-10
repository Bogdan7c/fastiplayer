# Сессия 15c2: подсказка зрителю, когда страница не выполнила выбор

Часть сессии 15c (решения 298, 300, 301). Зависит от 15c1 ([результат](results/15c1.md)).

> Выполни сессию 15c2 плана `user/browser-bridge/`: зритель выбирает озвучку (качество, Next) в окне плеера, а страница выбор не выполнила — плеер показывает причину вместо тишины. Механизм владелец подтвердил до кода (решение 301). Сначала прочитай `user/browser-bridge/README.md`, этот файл, `results/15c1.md`, `results/15b.md`, `results/14a.md`, решения 295, 298, 301.

## Механизм (решение 301)

1. Зритель выбирает озвучку в ♪. Плеер больше не пишет сразу «Audio: X», будто всё готово, а пишет «Switching audio on the page…».
2. Экстеншен жмёт кнопки страницы и узнаёт исход адаптера: `done`, `no_such_variant` (у новой озвучки нет этой серии, решение 295), `person_only` (сайт принимает только настоящую мышь), `failed`.
3. Новое сообщение протокола 1.8 «исход выбора» от страницы к плееру: вид меню, пункт, исход.
4. Плеер показывает причину по-английски («This dub has no such episode — pick an earlier episode on the page», подсказка «only on the page» для `person_only`). Возврат на ту же секунду (`position_carry`) при отказе снимается.
5. Старый экстеншен (протокол 1.7) исхода не шлёт: плеер ведёт себя как сейчас.

## Карта кода (разведка 15c1, сверить с текущим кодом)

**Протокол** `crates/browser-bridge-protocol`:
- версии — `src/version.rs` (`ProtocolVersion::V1_7`, `CURRENT_PROTOCOL_VERSION`, история в шапке); нужна `V1_8`;
- page→player — `src/session/page.rs`: вариант в `PageSessionMessage`, `stream_scope()` (`Unbound`, как `PageNavigation`), `introduced_in()`, `required_version()`, список известных типов; образец доменного типа — `src/session/page_navigation.rs` (`strict_label!`/`strict_object!`), экспорт — `src/session/mod.rs`;
- фикстуры Rust и TS — `fixtures/generate_fixtures.py` (образец `write_variant_protocol_messages()`, таблицы `MESSAGE_VERSIONS`), тесты `tests/fixtures.rs`, `tests/protocol_1_7.rs` (образец для `protocol_1_8.rs`), `tests/property.rs`.

**TS-зеркало** `extensions/browser/src/protocol/`: `version.ts`, `session/page.ts` (кодек), `session/introduced-in.ts`, `index.ts`; тесты `tests/protocol/protocol-1-7.test.ts` (образец).

**Фон экстеншена:** `src/background/handoff/variant-mirror.ts` — `command()` получает исход и сейчас только пишет заметку; `select()` отдаёт исход наблюдателю (`origin` `viewer`/`memory`: плееру — только выбор зрителя); `nextEpisode()` исход выбрасывает. Отказы до страницы («нет пункта в последнем меню», «нет следующей серии») — тоже исход. Отправка — как `page_navigation`: проверка `versionIncludes(agreedProtocol(), pageSessionIntroducedIn(…))`. Три нажатия Ashdi и «остаёмся» — `src/adapters/sites/playerjs/folder-dub-switch.ts`.

**Плеер** `crates/app-egui`:
- приём — `browser_session/translator.rs` (около строки 427, файл на пределе 800 строк: только замена, без роста) → `browser_session/translator/page_variants.rs` → `PageVariantUpdate`/`IntakeNotice` (`browser_session/intake_notice.rs`: исход нельзя вытеснять, как меню — нужно своё правило);
- выбор зрителя и тост — `browser_session/session_media/page_variants.rs` (`viewer_selects_page_variant`, `PageVariantNotice::Selected`, `arm_position_carry`), тексты тостов — `browser_session/page_variants.rs` (`toast_text`), показ — `state/window_mode/page_variants.rs` (ключ замены `TransientReplaceKey::AudioTrack`: причина заменит «Switching…»);
- `browser_session/session_media.rs` — 797 строк, новая логика туда не идёт.

**Тесты рядом:** `crates/app-egui/src/browser_session/**/tests/page_variants.rs`, `crates/app-egui/tests/browser_session_process/page_variants.rs`, `extensions/browser/tests/background/variant-mirror-session.test.ts`; стенд `crates/browser-bridge-bench/src/scenarios/series*` (исход выбора стенд сейчас не судит).

## Что сделать

1. Протокол 1.8: сообщение «исход выбора», фикстуры, тесты версий (старый экстеншен не шлёт, старый плеер не получает).
2. Экстеншен: исход выбора зрителя (озвучка, качество, Next) уходит плееру; выбор памяти — нет.
3. Плеер: «Switching … on the page…» вместо обещания, причина отказа, снятие `position_carry`; галочка в ♪ как и раньше — только по меню страницы.
4. Стенд: страница сериала, где у новой озвучки нет серии (решение 295), — плеер показывает причину; focused-тесты на каждый исход и на старую версию.
5. Живьём: klonua (сериал, озвучка без серии) в Chromium и Firefox; проверка владельцем.

## Критерий выхода

`results/15c2.md`, итог 15c — `results/15c.md`; строка «The player does not yet say why nothing changed» в `docs/browser-bridge/compatibility.md` снята. Memories: `mem:browser-bridge/core`.
