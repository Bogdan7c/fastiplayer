// Сборка передачи вкладки на настоящих API браузера (chrome.* есть и в Firefox MV3).
// Вся логика — в модулях этой папки; здесь только переходники к браузеру.

import { browserApi } from "../../shared/browser-api.ts";
import { HANDOFF_PORT_NAME, type PageCommand } from "../../shared/handoff-contract.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import type { FramePort } from "./frame-port.ts";
import type { ExecuteInAllFrames } from "./frame-survey.ts";
import { createHandOffCommandListener } from "./hand-off-command.ts";
import type { SessionDoor } from "./handoff-session.ts";
import type { SessionWebSocket } from "./session-socket.ts";
import { createTabHandoffs, type TabHandoffs } from "./tab-handoffs.ts";

/** Случайных байт в метке жизни фона (6 байт = 8 символов base64url). */
const BACKGROUND_NONCE_BYTES = 6;

/**
 * Метка этой жизни фона в ключе сеанса: после перезапуска service worker номера попыток начинаются
 * заново, и без метки новый сеанс мог бы получить ключ ещё живого старого (плеер вернул бы ему
 * тот же сеанс — `reused_existing`).
 */
function createBackgroundNonce(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(BACKGROUND_NONCE_BYTES));
  return btoa(String.fromCharCode(...bytes)).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

const executeInAllFrames: ExecuteInAllFrames = (tabId, func, args) =>
  browserApi.scripting.executeScript({ target: { tabId, allFrames: true }, func, args });

function connectFrame(tabId: number, frameId: number): FramePort {
  const port = browserApi.tabs.connect(tabId, { frameId, name: HANDOFF_PORT_NAME });
  return {
    postMessage: (command: PageCommand) => port.postMessage(command),
    disconnect: () => port.disconnect(),
    onMessage: { addListener: (listener) => port.onMessage.addListener((message: unknown) => listener(message)) },
    onDisconnect: {
      addListener: (listener) =>
        port.onDisconnect.addListener(() => {
          // Chrome требует прочитать lastError в обработчике, иначе пишет «Unchecked runtime.lastError»
          // (порт к фрейму без нашего скрипта закрывается именно так). Причина нам не нужна: уход есть уход.
          void browserApi.runtime.lastError;
          listener();
        }),
    },
  };
}

/** Включает команду «отдать вкладку N» и слежение за закрытием вкладок. */
export function installTabHandoffs(door: SessionDoor, scheduler: Scheduler): TabHandoffs {
  const nonce = createBackgroundNonce();
  const handoffs = createTabHandoffs({
    door,
    scheduler,
    createWebSocket: (url) => new WebSocket(url) as unknown as SessionWebSocket,
    connectFrame,
    executeInAllFrames,
    tabExists: (tabId) => browserApi.tabs.get(tabId).then(() => true, () => false),
    // Сеть фона для байтов `<video src>` (сессия 09b): права на сайты снимают CORS, куки сайта идут
    // с запросом. Глобальный `fetch` зовётся без `this` — иначе «Illegal invocation».
    fetchRange: (url, init) => fetch(url, init),
    describeTab: (tabId) =>
      browserApi.tabs.get(tabId).then(
        (tab) => ({ title: tab.title, url: tab.url }),
        () => undefined,
      ),
    sessionKeyFor: (tabId, attempt) => `t${tabId}-${attempt}-${nonce}`,
  });
  browserApi.runtime.onMessage.addListener(
    createHandOffCommandListener(
      { extensionId: browserApi.runtime.id, extensionBaseUrl: browserApi.runtime.getURL("") },
      (tabId) => handoffs.handOffTab(tabId),
    ),
  );
  browserApi.tabs.onRemoved.addListener((tabId) => handoffs.tabRemoved(tabId));
  return handoffs;
}
