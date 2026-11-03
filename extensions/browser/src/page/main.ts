// Скрипт MAIN-мира (общий мир со страницей), `document_start`, все фреймы. Точка входа IIFE `page.js`.
//
// Здесь только сборка: мост (`startPageBridge`, bridge.ts) и окружение тени (`shadow-assembly.ts`:
// оригиналы, переопределения прототипов, страж mediaSession) — всё на старте, до скриптов страницы.
// ВАЖНО: никаких глобалов и исключений в страницу — всё в замыкании и под try/catch.

import { startPageBridge } from "./bridge.ts";
import { installCodecPolicy } from "./codec-policy/index.ts";
import { createOddityReporter, type DebugSink } from "./oddity-reporter.ts";
import { captureShadowEnvironment, shadowStarterFor } from "./shadow-assembly.ts";

/**
 * Текст бандла хука выделенного воркера (`src/worker/main.ts`). Подставляет сборка
 * (scripts/build.ts, `define`): сначала собирается бандл воркера, затем его текст — строкой сюда.
 * В сборке Firefox — пустая строка: там MSE в воркере нет, и обёртка не нужна (решение 63).
 */
declare const __FASTIPLAYER_WORKER_HOOK_SOURCE__: string;

function main(win: Window & typeof globalThis, debugSink: DebugSink): void {
  // Ответы про кодеки (решения 43/64) — самыми первыми: страничный плеер спрашивает их с первой строки.
  // `codecPolicy.policy.current()` — синхронное чтение действующей политики: обёртка `Worker`
  // запекает её в хук каждого нового воркера.
  const codecPolicy = installCodecPolicy(win, createOddityReporter(debugSink));
  // Окружение тени — первым: его оригиналы снимаются до любых наших и чужих подмен прототипов.
  const shadowEnvironment = captureShadowEnvironment(win);
  startPageBridge(win, {
    startShadow: shadowStarterFor(shadowEnvironment),
    debugSink,
    // Пустой текст — сборка без хука (Firefox): обёртка `Worker` не ставится вовсе.
    ...(__FASTIPLAYER_WORKER_HOOK_SOURCE__.length > 0
      ? { workerHook: { source: __FASTIPLAYER_WORKER_HOOK_SOURCE__, policy: codecPolicy.policy } }
      : {}),
  });
}

// Консоль снимается первой, до всего, что может бросить: через неё же — след неудачного старта.
const consoleAtStart = window.console;
const debugAtStart = consoleAtStart.debug;
const debugSink: DebugSink = (line) => Reflect.apply(debugAtStart, consoleAtStart, [line]);
try {
  main(window, debugSink);
} catch {
  // Мост не встал — странице это не мешает; одна строка, чтобы при разборе было с чего начать.
  createOddityReporter(debugSink)("page bridge failed to start");
}
