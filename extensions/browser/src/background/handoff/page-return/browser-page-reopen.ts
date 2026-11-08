// Браузерная сторона возврата страницы на настоящих API (chrome.* есть и в Firefox MV3), сессия 11b2.
// Логика — в page-reopen-run.ts; здесь только переходники. Факты разведки — в шапке page-reopen-browser.ts.
//
// Прав манифеста не добавляет: `tabs.create/update/remove/query/get` и `windows.get/getAll` работают без
// права `tabs`, а адреса вкладок видны благодаря `host_permissions: <all_urls>` (manifest/PERMISSIONS.md).

import { browserApi } from "../../../shared/browser-api.ts";
import { ARM_NEXT_VIDEO_ACCEPTED, ARM_NEXT_VIDEO_KIND, type ArmNextVideoRequest } from "../../../shared/handoff-contract.ts";
import { createLateVideoReader, LATE_POSITION_TOLERANCE_S, visitFrameVideos } from "../../entries/late-video-reader.ts";
import { returnablePageOf } from "../handoff-session-dependencies.ts";
import type { PageReopenBrowser, ReopenedTab } from "./page-reopen-browser.ts";
import type { ReturnablePage } from "./page-return-memory.ts";

/** Чтение видео вкладки тем же внедрением, что у позднего входа (сессия 10d): длина — для причины отказа. */
const videoReader = createLateVideoReader((tabId, target) =>
  browserApi.scripting.executeScript({ target: { tabId, allFrames: true }, func: visitFrameVideos, args: [target, LATE_POSITION_TOLERANCE_S] }),
);

/** Окно годится для возврата: обычное окно браузера того же вида (инкогнито или нет). */
function windowFits(window: chrome.windows.Window, page: ReturnablePage): window is chrome.windows.Window & { id: number } {
  return window.id !== undefined && window.type === "normal" && window.incognito === page.incognito;
}

function tabIdOf(tab: chrome.tabs.Tab): number {
  if (tab.id === undefined) throw new Error("the browser returned a tab without an id");
  return tab.id;
}

export const browserPageReopen: PageReopenBrowser = {
  async findWindow(page) {
    const previous = await browserApi.windows.get(page.windowId).catch(() => undefined);
    if (previous !== undefined && windowFits(previous, page)) return previous.id;
    const windows = await browserApi.windows.getAll({ windowTypes: ["normal"] });
    const fitting = windows.filter((window) => windowFits(window, page));
    // Сначала окно, в котором зритель был последним: туда он и посмотрит.
    return (fitting.find((window) => window.focused) ?? fitting[0])?.id;
  },
  async findOpenTab(page) {
    const tabs = await browserApi.tabs.query({});
    return tabs.find((tab) => tab.url === page.url && tab.incognito === page.incognito && tab.id !== undefined)?.id;
  },
  async openTab(windowId, url) {
    // В фоне, потом активной: так Chromium не поднимает окно браузера поверх плеера (разведка 11b2).
    const tabId = tabIdOf(await browserApi.tabs.create({ windowId, url, active: false }));
    await browserApi.tabs.update(tabId, { muted: true });
    await browserApi.tabs.update(tabId, { active: true });
    const opened: ReopenedTab = { tabId, openedBy: "page_return", wasMuted: false };
    return opened;
  },
  async bringForwardMuted(tabId) {
    const tab = await browserApi.tabs.get(tabId);
    const wasMuted = tab.mutedInfo?.muted === true;
    await browserApi.tabs.update(tabId, { muted: true });
    await browserApi.tabs.update(tabId, { active: true });
    const reused: ReopenedTab = { tabId, openedBy: "viewer", wasMuted };
    return reused;
  },
  async setMuted(tabId, muted) {
    await browserApi.tabs.update(tabId, { muted });
  },
  async closeTab(tabId) {
    await browserApi.tabs.remove(tabId);
  },
  async armPageReturn(tabId, leaseMs, target) {
    const request: ArmNextVideoRequest = { kind: ARM_NEXT_VIDEO_KIND, lease_ms: leaseMs, page_return: target };
    // Получателя нет (страница грузится, страница ошибки) — промис отвергается: это «скрипта нет».
    const reply: unknown = await browserApi.tabs.sendMessage(tabId, request).catch(() => undefined);
    return reply === ARM_NEXT_VIDEO_ACCEPTED ? "armed" : "no_script";
  },
  async describeTab(tabId) {
    const tab = await browserApi.tabs.get(tabId).catch(() => undefined);
    return tab === undefined ? undefined : returnablePageOf({ url: tab.url, windowId: tab.windowId, incognito: tab.incognito });
  },
  async readVideos(tabId) {
    const facts = await videoReader.read(tabId);
    return facts.map((fact) => ({ durationS: fact.duration_s }));
  },
};
