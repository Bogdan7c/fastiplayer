// Контекстное меню и вход «видео/страница», кнопка поверх видео (сессия 10c). Итог — кто какое видео получил,
// что сказано человеку и что НЕ случилось (выключенный вход, чужой отправитель).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  createEntryMenus,
  isEntrySettingsChange,
  LINK_MENU_ID,
  MENU_DOCUMENT_PATTERNS,
  PAGE_MENU_ID,
  routeMenuClick,
  VIDEO_MENU_ID,
  type MenuClickTargets,
  type MenusApi,
} from "../../src/background/entries/entry-menus.ts";
import { createMenuEntry } from "../../src/background/entries/menu-entry.ts";
import { createOverlayEntryListener } from "../../src/background/entries/overlay-entry.ts";
import type { EntryReporter } from "../../src/background/entries/entry-reporter.ts";
import type { EntryResult } from "../../src/background/entries/entry-report-text.ts";
import type { HandOffTabOutcome, VideoRef } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_ENTRY_SETTINGS, ENTRY_SETTINGS_KEY, type EntrySettings } from "../../src/shared/entry-settings.ts";

const ALL_ON: EntrySettings = { video_menu: true, link_menu: true, overlay_button: true };

function fakeMenus(options: { failSetVisibleFor?: string; failCreateFor?: string; failRemoveAll?: boolean } = {}) {
  const created: Parameters<MenusApi["create"]>[0][] = [];
  const visibility = new Map<string, boolean>();
  let removedAll = 0;
  const api: MenusApi = {
    removeAll: () => {
      if (options.failRemoveAll === true) return Promise.reject(new Error("removeAll failed"));
      removedAll += 1;
      created.length = 0;
      visibility.clear();
      return Promise.resolve();
    },
    create: (item) => {
      if (item.id === options.failCreateFor) return Promise.reject(new Error("duplicate id"));
      created.push(item);
      visibility.set(item.id, item.visible);
      return Promise.resolve();
    },
    setVisible: (id, visible) => {
      if (id === options.failSetVisibleFor) return Promise.reject(new Error("no such menu item"));
      visibility.set(id, visible);
      return Promise.resolve();
    },
  };
  return { api, created, visibility, removedAll: () => removedAll };
}

describe("создание пунктов меню", () => {
  it("removeAll, затем три пункта со своими контекстами, ссылка — только http(s), все — на обычных страницах", async () => {
    const menus = fakeMenus();
    await createEntryMenus(menus.api, () => {}).installAll(DEFAULT_ENTRY_SETTINGS);
    assert.equal(menus.removedAll(), 1);
    assert.deepEqual(
      menus.created.map((item) => [item.id, item.title, item.contexts, item.targetUrlPatterns ?? null]),
      [
        [VIDEO_MENU_ID, "Play in Fastiplayer", ["video"], null],
        [PAGE_MENU_ID, "Play video from this page in Fastiplayer", ["page", "frame"], null],
        [LINK_MENU_ID, "Play link in Fastiplayer", ["link"], ["http://*/*", "https://*/*"]],
      ],
    );
    for (const item of menus.created) assert.deepEqual(item.documentUrlPatterns, ["http://*/*", "https://*/*", "file:///*"]);
    assert.deepEqual(MENU_DOCUMENT_PATTERNS, ["http://*/*", "https://*/*", "file:///*"]);
  });

  it("сбой создания одного пункта не отменяет остальные и отмечается; сбой removeAll тоже не останавливает создание", async () => {
    const menus = fakeMenus({ failCreateFor: PAGE_MENU_ID, failRemoveAll: true });
    const notes: string[] = [];
    await createEntryMenus(menus.api, (message) => void notes.push(message)).installAll(DEFAULT_ENTRY_SETTINGS);
    assert.deepEqual(menus.created.map((item) => item.id), [VIDEO_MENU_ID, LINK_MENU_ID]);
    assert.deepEqual(notes, ["could not clear the old context menu items", `could not create the menu item ${PAGE_MENU_ID}`]);
  });

  it("видимость при создании следует настройке: выключенная группа создаётся скрытой", async () => {
    const menus = fakeMenus();
    await createEntryMenus(menus.api, () => {}).installAll({ video_menu: false, link_menu: true, overlay_button: false });
    assert.deepEqual([...menus.visibility], [[VIDEO_MENU_ID, false], [PAGE_MENU_ID, false], [LINK_MENU_ID, true]]);
  });

  it("смена настройки показывает и скрывает пункты сразу, без пересоздания; пункт на странице относится к video_menu", async () => {
    const menus = fakeMenus();
    const entryMenus = createEntryMenus(menus.api, () => {});
    await entryMenus.installAll(ALL_ON);
    await entryMenus.applySettings({ video_menu: false, link_menu: true, overlay_button: true });
    assert.deepEqual([...menus.visibility], [[VIDEO_MENU_ID, false], [PAGE_MENU_ID, false], [LINK_MENU_ID, true]]);
    await entryMenus.applySettings({ video_menu: true, link_menu: false, overlay_button: true });
    assert.deepEqual([...menus.visibility], [[VIDEO_MENU_ID, true], [PAGE_MENU_ID, true], [LINK_MENU_ID, false]]);
    assert.equal(menus.removedAll(), 1, "пункты не пересоздавались");
  });

  it("сбой одного пункта не мешает остальным и отмечается для разработчика", async () => {
    const menus = fakeMenus({ failSetVisibleFor: VIDEO_MENU_ID });
    const notes: string[] = [];
    await createEntryMenus(menus.api, (message) => void notes.push(message)).applySettings({ video_menu: false, link_menu: false, overlay_button: false });
    assert.equal(menus.visibility.get(PAGE_MENU_ID), false);
    assert.equal(menus.visibility.get(LINK_MENU_ID), false);
    assert.deepEqual(notes, [`could not update the visibility of ${VIDEO_MENU_ID}`]);
  });

  it("storage.onChanged: наш ключ в области local — да, остальное — нет", () => {
    assert.equal(isEntrySettingsChange("local", { [ENTRY_SETTINGS_KEY]: {} }), true);
    assert.equal(isEntrySettingsChange("session", { [ENTRY_SETTINGS_KEY]: {} }), false);
    assert.equal(isEntrySettingsChange("local", { codecSetting: {} }), false);
  });
});

function fakeTargets() {
  const calls: unknown[] = [];
  const targets: MenuClickTargets = {
    playVideo: (tabId, frameId) => (calls.push(["video", tabId, frameId]), Promise.resolve()),
    playLink: (linkUrl, source) => (calls.push(["link", linkUrl, source]), Promise.resolve()),
  };
  return { targets, calls };
}

describe("разбор клика по пункту", () => {
  it("пункт на видео и пункт на странице идут в вход «видео» с фреймом клика (нет фрейма — верхний)", async () => {
    const { targets, calls } = fakeTargets();
    await routeMenuClick({ menuItemId: VIDEO_MENU_ID, frameId: 3 }, { id: 7 }, ALL_ON, targets);
    await routeMenuClick({ menuItemId: PAGE_MENU_ID }, { id: 7 }, ALL_ON, targets);
    assert.deepEqual(calls, [["video", 7, 3], ["video", 7, 0]]);
  });

  it("пункт на ссылке идёт во вход «ссылка» с адресом и положением исходной вкладки", async () => {
    const { targets, calls } = fakeTargets();
    await routeMenuClick({ menuItemId: LINK_MENU_ID, linkUrl: "https://a.example/v" }, { id: 7, windowId: 2, index: 4 }, ALL_ON, targets);
    assert.deepEqual(calls, [["link", "https://a.example/v", { tabId: 7, windowId: 2, index: 4 }]]);
  });

  it("выключенный вход не срабатывает даже по клику (гонка с меню), и каждая группа выключается отдельно", async () => {
    const { targets, calls } = fakeTargets();
    await routeMenuClick({ menuItemId: VIDEO_MENU_ID }, { id: 1 }, { ...ALL_ON, video_menu: false }, targets);
    await routeMenuClick({ menuItemId: PAGE_MENU_ID }, { id: 1 }, { ...ALL_ON, video_menu: false }, targets);
    await routeMenuClick({ menuItemId: LINK_MENU_ID, linkUrl: "https://a.example/" }, { id: 1 }, { ...ALL_ON, link_menu: false }, targets);
    assert.deepEqual(calls, []);
    await routeMenuClick({ menuItemId: VIDEO_MENU_ID }, { id: 1 }, { ...ALL_ON, link_menu: false }, targets);
    assert.equal(calls.length, 1);
  });

  it("клик без вкладки и чужой пункт — ничего", async () => {
    const { targets, calls } = fakeTargets();
    await routeMenuClick({ menuItemId: VIDEO_MENU_ID }, undefined, ALL_ON, targets);
    await routeMenuClick({ menuItemId: "someone-else" }, { id: 1 }, ALL_ON, targets);
    assert.deepEqual(calls, []);
  });
});

function fakeReporter() {
  const reports: { tabId: number; result: EntryResult }[] = [];
  const reporter: EntryReporter = { report: (tabId, result) => (reports.push({ tabId, result }), Promise.resolve()), forgetProblem: () => Promise.resolve() };
  return { reporter, reports };
}

const STARTED: HandOffTabOutcome = { kind: "started", player_window: "new" };

describe("вход «правый клик по видео или странице»", () => {
  it("видео названо — именно оно уходит в передачу, с фреймом клика; итог идёт в репортёр", async () => {
    const { reporter, reports } = fakeReporter();
    const handOffs: { tabId: number; video: VideoRef | undefined }[] = [];
    const entry = createMenuEntry({
      askRightClickedVideo: (_tabId, frameId) => Promise.resolve(frameId === 2 ? { document: "doc-9", element: 5 } : null),
      handOffTab: (tabId, video) => (handOffs.push({ tabId, video }), Promise.resolve(STARTED)),
      reporter,
      note: () => {},
    });
    await entry.playVideo(7, 2);
    assert.deepEqual(handOffs, [{ tabId: 7, video: { frame_id: 2, document: "doc-9", element: 5 } }]);
    assert.deepEqual(reports, [{ tabId: 7, result: { kind: "outcome", outcome: STARTED } }]);
  });

  it("сайт забрал клик (видео не названо) — отдаётся главное видео вкладки", async () => {
    const { reporter } = fakeReporter();
    const handOffs: (VideoRef | undefined)[] = [];
    await createMenuEntry({
      askRightClickedVideo: () => Promise.resolve(null),
      handOffTab: (_tabId, video) => (handOffs.push(video), Promise.resolve(STARTED)),
      reporter,
      note: () => {},
    }).playVideo(7, 0);
    assert.deepEqual(handOffs, [undefined]);
  });

  it("фрейм не ответил (executeScript отклонён) — главное видео, сбой отмечен, вход не падает", async () => {
    const { reporter, reports } = fakeReporter();
    const notes: string[] = [];
    const handOffs: (VideoRef | undefined)[] = [];
    await createMenuEntry({
      askRightClickedVideo: () => Promise.reject(new Error("Frame with ID 3 was removed")),
      handOffTab: (_tabId, video) => (handOffs.push(video), Promise.resolve({ kind: "no_video" })),
      reporter,
      note: (message) => void notes.push(message),
    }).playVideo(7, 3);
    assert.deepEqual(handOffs, [undefined]);
    assert.equal(notes.length, 1);
    assert.deepEqual(reports[0]?.result, { kind: "outcome", outcome: { kind: "no_video" } }, "человек узнаёт, что видео нет");
  });

  it("команда передачи бросила — человеку приходит failed, а не тишина", async () => {
    const { reporter, reports } = fakeReporter();
    await createMenuEntry({
      askRightClickedVideo: () => Promise.resolve(null),
      handOffTab: () => Promise.reject(new Error("boom")),
      reporter,
      note: () => {},
    }).playVideo(7, 0);
    assert.equal(reports[0]?.result.kind, "outcome");
    assert.equal((reports[0]?.result as { outcome: HandOffTabOutcome }).outcome.kind, "failed");
  });
});

describe("вход «кнопка поверх видео»", () => {
  const PRESSED = { kind: "overlay_button_pressed", document: "doc-1", element: 3 };
  const GOOD_SENDER = { id: "ext-id", tab: { id: 7 }, frameId: 4 };

  function setupOverlay(settings: EntrySettings) {
    const { reporter, reports } = fakeReporter();
    const handOffs: { tabId: number; video: VideoRef }[] = [];
    const listener = createOverlayEntryListener({
      extensionId: "ext-id",
      readSettings: () => Promise.resolve(settings),
      handOffTab: (tabId, video) => (handOffs.push({ tabId, video }), Promise.resolve(STARTED)),
      reporter,
      note: () => {},
    });
    const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));
    return { listener, handOffs, reports, settle };
  }

  it("кнопка включена: видео собирается из метки/ключа страницы и фрейма отправителя (его называет браузер), итог в репортёр", async () => {
    const { listener, handOffs, reports, settle } = setupOverlay(ALL_ON);
    assert.equal(listener(PRESSED, GOOD_SENDER), false);
    await settle();
    assert.deepEqual(handOffs, [{ tabId: 7, video: { frame_id: 4, document: "doc-1", element: 3 } }]);
    assert.deepEqual(reports, [{ tabId: 7, result: { kind: "outcome", outcome: STARTED } }]);
  });

  it("кнопка выключена в настройках: передачи нет и плашки нет — молчание", async () => {
    const { listener, handOffs, reports, settle } = setupOverlay({ ...ALL_ON, overlay_button: false });
    listener(PRESSED, GOOD_SENDER);
    await settle();
    assert.deepEqual(handOffs, []);
    assert.deepEqual(reports, []);
  });

  it("чужой отправитель, сообщение не из вкладки, без фрейма или кривой формы — не обрабатывается", async () => {
    const { listener, handOffs, settle } = setupOverlay(ALL_ON);
    listener(PRESSED, { ...GOOD_SENDER, id: "other-extension" });
    listener(PRESSED, { id: "ext-id", frameId: 0 });
    listener(PRESSED, { id: "ext-id", tab: { id: 7 } });
    listener({ ...PRESSED, element: 0 }, GOOD_SENDER);
    listener({ ...PRESSED, frame_id: 9 }, GOOD_SENDER);
    listener({ kind: "hand_off_tab", tab_id: 7 }, GOOD_SENDER);
    await settle();
    assert.deepEqual(handOffs, []);
  });
});

