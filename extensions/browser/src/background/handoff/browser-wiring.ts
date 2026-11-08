// Сборка передачи вкладки на настоящих API браузера (chrome.* есть и в Firefox MV3).
// Вся логика — в модулях этой папки; здесь только переходники к браузеру.

import { browserApi } from "../../shared/browser-api.ts";
import { ARM_NEXT_VIDEO_KIND, HANDOFF_PORT_NAME, type ArmNextVideoRequest, type PageCommand } from "../../shared/handoff-contract.ts";
import { ARM_AD_WATCH_KIND, PRESS_AD_SKIP_KIND, type ArmAdWatchRequest, type PressAdSkipRequest } from "../../shared/ad-watch-contract.ts";
import { parsePressSkipOutcome } from "../../shared/ad-watch-validation.ts";
import { readLocalAddressSetting } from "../../shared/local-address-setting.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import type { FramePort } from "./frame-port.ts";
import type { ExecuteInAllFrames } from "./frame-survey.ts";
import { createFileReferrerRules } from "./file-referrer-rule.ts";
import { createHandOffCommandListener } from "./hand-off-command.ts";
import { createSurveyTabCommandListener } from "./survey-command.ts";
import type { AdMirrorBrowser } from "./ad-skip-mirror.ts";
import type { SessionDoor } from "./handoff-session.ts";
import { createLocalAddressBlock, localAddressBlockRules } from "./local-address-block-rules.ts";
import { createSessionRules, type SessionRulesApi } from "./session-rules.ts";
import type { SessionWebSocket } from "./session-socket.ts";
import type { TabModel } from "../tab-model/tab-model.ts";
import { createTabHandoffs, type TabHandoffs } from "./tab-handoffs.ts";
import { browserPageReopen } from "./page-return/browser-page-reopen.ts";

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

/**
 * Взвести сторож следующего видео: без `frameId` сообщение получают все фреймы вкладки. Фрейма без
 * нашего скрипта или закрытой вкладки достаточно для отказа промиса: следующее продление попробует снова.
 */
function armNextVideo(tabId: number, leaseMs: number): void {
  const request: ArmNextVideoRequest = { kind: ARM_NEXT_VIDEO_KIND, lease_ms: leaseMs };
  browserApi.tabs.sendMessage(tabId, request).catch(() => {});
}

/**
 * Браузерная сторона дублёра кнопки пропуска (09d). Аренда уходит всем фреймам вкладки; нажатие — одному
 * фрейму (`frameId`); вывод вперёд — `tabs.update` и `windows.update` без новых прав манифеста.
 */
export const adMirrorBrowser: AdMirrorBrowser = {
  armAdWatch(tabId, leaseMs) {
    const request: ArmAdWatchRequest = { kind: ARM_AD_WATCH_KIND, lease_ms: leaseMs };
    // Фрейма без нашего скрипта или закрытой вкладки достаточно для отказа: следующее продление попробует снова.
    browserApi.tabs.sendMessage(tabId, request).catch(() => {});
  },
  async pressSkipInFrame(tabId, frameId) {
    const request: PressAdSkipRequest = { kind: PRESS_AD_SKIP_KIND };
    const reply: unknown = await browserApi.tabs.sendMessage(tabId, request, { frameId });
    const outcome = parsePressSkipOutcome(reply);
    if (outcome === undefined) throw new Error("the frame gave an unknown answer to press_ad_skip");
    return outcome;
  },
};

/**
 * Сессионные правила браузера. Форма наших правил уже проверена типами модуля, браузер хочет свои
 * перечисления; в Firefox и Chrome это те же строки. API нет — вызов бросит, модуль это переживает.
 */
const sessionRulesApi: SessionRulesApi = {
  updateSessionRules: (options) =>
    browserApi.declarativeNetRequest.updateSessionRules(options as unknown as chrome.declarativeNetRequest.UpdateRuleOptions),
  getSessionRules: () => browserApi.declarativeNetRequest.getSessionRules(),
};

/** Включает команду «отдать вкладку N» и слежение за закрытием вкладок. */
export function installTabHandoffs(door: SessionDoor, scheduler: Scheduler, tabModel: TabModel): TabHandoffs {
  const nonce = createBackgroundNonce();
  // Хост адреса страниц экстеншена: инициатор его запросов. В Firefox это внутренний UUID, а не `runtime.id`.
  const extensionHost = new URL(browserApi.runtime.getURL("")).hostname;
  // Создаётся на каждом старте фона: снимает остатки прошлой жизни service worker и ставит заслон.
  const sessionRules = createSessionRules({
    api: sessionRulesApi,
    scheduler,
    permanentRules: localAddressBlockRules(extensionHost),
    reportFailureOnce: (error) => console.warn("[fastiplayer] a network rule failed, the Referer or the local address block may be missing:", error),
  });
  const handoffs = createTabHandoffs({
    door,
    scheduler,
    createWebSocket: (url) => new WebSocket(url) as unknown as SessionWebSocket,
    connectFrame,
    armNextVideo,
    adMirrorBrowser,
    noteForDeveloper: (message) => console.debug(`[fastiplayer] ${message}`),
    executeInAllFrames,
    tabModel,
    tabExists: (tabId) => browserApi.tabs.get(tabId).then(() => true, () => false),
    // Сеть фона для байтов `<video src>` (сессия 09b): права на сайты снимают CORS, куки сайта идут
    // с запросом. Глобальный `fetch` зовётся без `this` — иначе «Illegal invocation».
    fetchRange: (url, init) => fetch(url, init),
    fileReferrer: createFileReferrerRules(sessionRules),
    localAddressBlock: createLocalAddressBlock(sessionRules, extensionHost),
    readLocalAddressSetting: () => readLocalAddressSetting({ get: (keys) => browserApi.storage.local.get([...keys]), set: (items) => browserApi.storage.local.set(items) }),
    reportSettingFailure: (error) => console.warn("[fastiplayer] could not read the local address setting, blocking:", error),
    // Причина флуда — только в журнал разработчика, модель вкладки хранит закрытый вид.
    onSessionEnded: (_tabId, end) => {
      if (end.kind === "page_flood") console.debug(`[fastiplayer] page exceeded the event budget: ${end.overBudget}`);
    },
    describeTab: (tabId) =>
      browserApi.tabs.get(tabId).then(
        (tab) => ({ title: tab.title, url: tab.url, windowId: tab.windowId, incognito: tab.incognito }),
        () => undefined,
      ),
    pageReopen: browserPageReopen,
    extensionVersion: browserApi.runtime.getManifest().version,
    sessionKeyFor: (tabId, attempt) => `t${tabId}-${attempt}-${nonce}`,
  });
  browserApi.runtime.onMessage.addListener(
    createHandOffCommandListener(
      { extensionId: browserApi.runtime.id, extensionBaseUrl: browserApi.runtime.getURL("") },
      (tabId, consent, video) => handoffs.handOffTab(tabId, consent, video),
    ),
  );
  browserApi.runtime.onMessage.addListener(
    createSurveyTabCommandListener(
      { extensionId: browserApi.runtime.id, extensionBaseUrl: browserApi.runtime.getURL("") },
      (tabId) => handoffs.surveyTab(tabId),
    ),
  );
  // Слушатели донесений сторожа и рекламы ставит режим 2 (auto-handoff/browser-wiring.ts): донесения идут через
  // его маршрутизатор, а проба рекламы читает те же донесения у вкладок без сеанса.
  browserApi.tabs.onRemoved.addListener((tabId) => {
    handoffs.tabRemoved(tabId);
    // Снимок закрытой вкладки стираем; `forgetTab` не бросает (сбой хранилища гасится в модели).
    void tabModel.forgetTab(tabId);
  });
  return handoffs;
}
