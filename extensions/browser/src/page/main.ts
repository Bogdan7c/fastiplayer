// Скрипт MAIN-мира (общий мир со страницей), `document_start`, все фреймы. Точка входа IIFE `page.js`.
//
// Здесь только сборка: мост (`startPageBridge`, bridge.ts) и окружение тени (`shadow-assembly.ts`:
// оригиналы, переопределения прототипов, страж mediaSession) — всё на старте, до скриптов страницы.
// ВАЖНО: никаких глобалов и исключений в страницу — всё в замыкании и под try/catch.

import { startPageBridge } from "./bridge.ts";
import { installCodecPolicy } from "./codec-policy/index.ts";
import { createOddityReporter, type DebugSink } from "./oddity-reporter.ts";
import { captureShadowEnvironment, heldShadowStarterFor, playWatchSeamFor, shadowStarterFor } from "./shadow-assembly.ts";
import { createPageRecording } from "./recorder-seam.ts";
import { scriptArrivalOf } from "./script-arrival.ts";

/**
 * Текст бандла хука выделенного воркера (`src/worker/main.ts`). Подставляет сборка
 * (scripts/build.ts, `define`): сначала собирается бандл воркера, затем его текст — строкой сюда.
 * В сборке Firefox — пустая строка: там MSE в воркере нет, и обёртка не нужна (решение 63).
 */
declare const __FASTIPLAYER_WORKER_HOOK_SOURCE__: string;

function main(win: Window & typeof globalThis, debugSink: DebugSink): void {
  // Самым первым: дальше страница и наши же обёртки уже могут что-то поменять.
  const scriptArrival = scriptArrivalOf(win.document.readyState, win.location.href);
  // Ответы про кодеки (решения 43/64) — самыми первыми: страничный плеер спрашивает их с первой строки.
  // `codecPolicy.policy.current()` — синхронное чтение действующей политики: обёртка `Worker`
  // запекает её в хук каждого нового воркера.
  const codecPolicy = installCodecPolicy(win, createOddityReporter(debugSink));
  // Окружение тени — первым: его оригиналы снимаются до любых наших и чужих подмен прототипов.
  const shadowEnvironment = captureShadowEnvironment(win);
  // Запись потока страницы: в продуктовой сборке шов возвращает `null` (см. recorder-seam.ts).
  const recording = createPageRecording(win, createOddityReporter(debugSink));
  const bridge = startPageBridge(win, {
    startShadow: shadowStarterFor(shadowEnvironment),
    startHeldShadow: heldShadowStarterFor(shadowEnvironment),
    playWatch: playWatchSeamFor(shadowEnvironment),
    now: shadowEnvironment?.now ?? (() => Number.NEGATIVE_INFINITY),
    debugSink,
    scriptArrival,
    ...(recording === null ? {} : { handoffTap: recording.tap, workerChannelExtension: recording.channelExtension }),
    // Пустой текст — сборка без хука (Firefox): обёртка `Worker` не ставится вовсе.
    ...(__FASTIPLAYER_WORKER_HOOK_SOURCE__.length > 0
      ? { workerHook: { source: __FASTIPLAYER_WORKER_HOOK_SOURCE__, policy: codecPolicy.policy } }
      : {}),
  });
  // Обёртки записи — после хука моста и теневых переопределений (они должны быть снаружи).
  recording?.install(bridge.tracker);
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
