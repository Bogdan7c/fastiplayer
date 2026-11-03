// Точка входа хука выделенного воркера (сессия 09b, пакет C). Отдельный IIFE-бандл: сборка
// (scripts/build.ts) кладёт его ТЕКСТОМ в page.js, а обёртка `Worker` MAIN-мира ставит его в
// начало blob-скрипта воркера, обернув в функцию с параметром-настройкой:
//
//   (function (fastiplayerWorkerHookConfig, fastiplayerWorkerHookLinks) { <этот бандл> })("<политика кодеков>", links);
//
// Поэтому имена ниже — свободные: бандл видит параметры обёртки лексически (см.
// src/shared/worker-hook-config.ts). Вне обёртки (если бандл подключить как есть) имён нет —
// `typeof` даёт "undefined", и хук честно ставится с прозрачной политикой и без донесения
// «скрипт сайта загружен» (MAIN-мир тогда отпустит blob-адрес сайта по своему сроку).

import { createOddityReporter, type DebugSink } from "../page/oddity-reporter.ts";
import { decodeWorkerHookConfig, WORKER_HOOK_SITE_LOADED_FIELD } from "../shared/worker-hook-config.ts";
import { installWorkerHook, type WorkerHookScope } from "./worker-hook.ts";
import { workerRecordingExtension } from "./recorder-seam.ts";

declare const fastiplayerWorkerHookConfig: unknown;
declare const fastiplayerWorkerHookLinks: unknown;

// Консоль снимается первой, до всего, что может бросить.
const consoleAtStart = self.console;
const debugAtStart = consoleAtStart.debug;
const debugSink: DebugSink = (line) => Reflect.apply(debugAtStart, consoleAtStart, [line]);
const reportOddity = createOddityReporter(debugSink);
try {
  const configValue: unknown = typeof fastiplayerWorkerHookConfig === "undefined" ? "" : fastiplayerWorkerHookConfig;
  const installation = installWorkerHook(self as unknown as WorkerHookScope, decodeWorkerHookConfig(configValue), reportOddity, workerRecordingExtension);
  const links: unknown = typeof fastiplayerWorkerHookLinks === "undefined" ? undefined : fastiplayerWorkerHookLinks;
  // Объект-ссылку создаёт наш же текст blob-скрипта: в него кладём донесение «скрипт сайта загружен».
  if (typeof links === "object" && links !== null) {
    Object.defineProperty(links, WORKER_HOOK_SITE_LOADED_FIELD, { value: installation.reportSiteScriptLoaded });
  }
} catch {
  // Хук не встал — коду сайта это не мешает; одна строка, чтобы при разборе было с чего начать.
  reportOddity("worker hook failed to start");
}
