// Скрипт MAIN-мира (общий мир со страницей), `document_start`, все фреймы. Точка входа IIFE `page.js`.
//
// Здесь только сборка: мост (`startPageBridge`, bridge.ts) и окружение тени (`shadow-assembly.ts`:
// оригиналы, переопределения прототипов, страж mediaSession) — всё на старте, до скриптов страницы.
// ВАЖНО: никаких глобалов и исключений в страницу — всё в замыкании и под try/catch.

import { startPageBridge } from "./bridge.ts";
import { createOddityReporter, type DebugSink } from "./oddity-reporter.ts";
import { captureShadowEnvironment, shadowStarterFor } from "./shadow-assembly.ts";

function main(win: Window & typeof globalThis, debugSink: DebugSink): void {
  // Окружение тени — первым: его оригиналы снимаются до любых наших и чужих подмен прототипов.
  const shadowEnvironment = captureShadowEnvironment(win);
  startPageBridge(win, { startShadow: shadowStarterFor(shadowEnvironment), debugSink });
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
