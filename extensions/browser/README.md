# Fastiplayer Bridge — браузерный экстеншен

Одна кодовая база на TypeScript, два манифеста (Chrome MV3 и Firefox MV3). Задуман и описан в `user/browser-bridge/`.
Пользовательская справка: `docs/browser-bridge/README.md`.

## Команды (из `extensions/browser/`)

| Команда | Что делает |
| --- | --- |
| `npm ci` | Ставит зависимости ровно по `package-lock.json`. |
| `npm run typecheck` | `tsc --noEmit`: строгая проверка типов кода, тестов и скриптов. |
| `npm test` | Тесты на встроенном `node:test` (Node 26 запускает `.ts` напрямую). |
| `npm run build` | Собирает `dist/chrome/` и `dist/firefox/`. |
| `npm run check` | Всё вместе. |
| `scripts/make-icons.sh` | Перегенерирует PNG-иконки из `docs/assets/fastiplayer-mark.svg` (нужен `rsvg-convert`). |

Правило для `.ts`: только «стираемый» синтаксис (без `enum`, `namespace`, parameter properties), импорты с расширением `.ts`.

## Раскладка

```
src/protocol/   типы протокола и строгий разбор (зеркало crates/browser-bridge-protocol)
src/background/ фон: поставщик статуса (шов), значок, маршрутизатор порта попапа; host-link/ — связь с посредником
src/content/    изолированный контент-скрипт (каркас)
src/page/       скрипт MAIN-мира (каркас; перехват MSE — сессия 09)
src/popup/      попап: статус -> текст (чистая функция) + тонкий DOM
src/options/    страница настроек (пока пустая)
src/adapters/   адаптеры сайтов (сессия 14)
src/shared/     общее: BridgeStatus, внутренние сообщения, доступ к API браузера, имя хоста
static/         html и иконки — копируются в dist как есть
manifest/       base.json + chrome.json / firefox.json (различия) + PERMISSIONS.md
scripts/        build.ts (esbuild), manifest.ts (сборка манифеста), make-icons.sh
tests/          *.test.ts
```

## Как собирается

- Каждая часть (background, content, page, popup, options) — отдельный IIFE-бандл esbuild без минификации и sourcemap.
- Манифест = `manifest/base.json` + различия браузера (слияние падает, если различия пытаются переопределить базу)
  + `version` из `package.json` + идентичность. Ключ Chrome и gecko id берутся ТОЛЬКО из
  `crates/browser-bridge-protocol/fixtures/extension-identity.json`.
- В корне `dist/<браузер>/` лежат `manifest.json` и ровно то, на что он ссылается (тест это проверяет) — каталог готов к упаковке в `.crx`/`.xpi` (сессия 07a).
- Сборка детерминирована: повторный запуск даёт те же байты.
- Минимумы: Chrome 148 (ключ манифеста `message_serialization: structured_clone` — байты видео идут в фон без base64; WebSocket продлевает жизнь service worker с 116), Firefox 140 (`data_collection_permissions`; `world: "MAIN"` есть с 128).

## Зависимости (только dev)

- `typescript` — проверка типов (`tsc --noEmit`).
- `esbuild` — бандлы IIFE.
- `@types/chrome`, `@types/node` — типы API браузера и Node.
- `web-ext` — запуск и линт Firefox-сборки (`npx web-ext lint -s dist/firefox`, `web-ext run`).

В выпускной код экстеншена зависимостей нет.

## Отладочная сборка записи потока страницы (`--stream-recorder`)

Только для разработки (сессия 09b2): пишет всё, что страница делает с MSE и видеоэлементом, и всё, что мост
отправил плееру, в файл `*.fprec` (формат — `src/page/stream-recorder/format.ts`). В продуктовой сборке
этого кода нет ни в каком виде; байты сайта попадают на диск только по явной выгрузке и в git не коммитятся.

```sh
npm run build -- --stream-recorder      # dist-stream-recorder/{chrome,firefox}; обычный dist/ не трогается
```

Из отладочного протокола (RDP evaluate в Firefox, CDP `Runtime.evaluate` в Chromium; для Chromium
`awaitPromise: true`, `userGesture: true` и `Browser.setDownloadBehavior` с каталогом):

```js
JSON.stringify(window.__fastiplayerStreamRecorder.summary())      // идёт ли запись: events, appended_bytes, truncated, workers
window.__fastiplayerStreamRecorder.exportRecording()              // Promise: скачивает *.fprec, возвращает {filename, size_bytes, events, truncated, ...}
window.dispatchEvent(new CustomEvent("fastiplayer-stream-recorder-export"))   // то же без ожидания результата
```
