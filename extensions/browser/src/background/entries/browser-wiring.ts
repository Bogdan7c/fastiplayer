// Сборка входов 10c на настоящих API браузера: контекстное меню, кнопка поверх видео, вход по ссылке, плашка
// и вкладочный значок. Только переходники к `chrome.*`: вся логика — в соседних модулях и проверяется
// через швы без браузера. Все слушатели ставятся СИНХРОННО на верхнем уровне (фон может спать, и
// браузер будит его именно по уже зарегистрированному слушателю).

import { browserApi, isFirefox } from "../../shared/browser-api.ts";
import { createMenuItem } from "./menu-create.ts";
import { RIGHT_CLICKED_VIDEO_FUNCTION_NAME, SHOW_ENTRY_RESULT_KIND } from "../../shared/entry-contract.ts";
import { parseRightClickedVideo } from "../../shared/entry-validation.ts";
import { entrySettingsFromChange, readEntrySettingsOrDefaults } from "../../shared/entry-settings.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import type { TabHandoffs } from "../handoff/tab-handoffs.ts";
import { createEntryMenus, isEntrySettingsChange, routeMenuClick, type MenusApi } from "./entry-menus.ts";
import { createEntryReporter, type EntryBadge, type EntryBanner, type EntryReporter } from "./entry-reporter.ts";
import { createLateEntryCommandListener } from "./late-entry-command.ts";
import { createLateEntry, type LateTabsApi } from "./late-entry.ts";
import { browserAnswersOfEachFrame } from "../frame-injection/browser-frame-injection.ts";
import { createLateVideoReader, LATE_POSITION_TOLERANCE_S, LATE_VIDEO_VISIT_DEADLINE_MS, visitFrameVideos, type ExecuteVideoVisit } from "./late-video-reader.ts";
import { createLinkEntry, type LinkTabsApi } from "./link-entry.ts";
import { createMenuEntry, type AskRightClickedVideo } from "./menu-entry.ts";
import { createOverlayEntryListener } from "./overlay-entry.ts";

/** Цвет вкладочного «!» — тот же, что у значка статуса связи (`badge.ts`). */
const PROBLEM_BADGE_COLOR = "#d93025";

/** Только для разработчика: проглоченный сбой не должен пропасть без следа. */
const note = (message: string, error: unknown): void => console.debug(`[fastiplayer] ${message}:`, error);

/**
 * Внедряемая функция: зовёт глобал изолированного мира по имени. Браузер сериализует её текст, поэтому
 * она самодостаточна (никаких импортов и замыканий).
 */
function callIsolatedGlobal(functionName: string): unknown {
  const call: unknown = (globalThis as Record<string, unknown>)[functionName];
  return typeof call === "function" ? call() : null;
}

const storeOfBrowser = {
  get: (keys: readonly string[]) => browserApi.storage.local.get([...keys]),
  set: (items: Record<string, unknown>) => browserApi.storage.local.set(items),
};

const menusApi: MenusApi = {
  removeAll: () => browserApi.contextMenus.removeAll(),
  create: (item) =>
    createMenuItem(
      browserApi.contextMenus,
      {
        id: item.id,
        title: item.title,
        contexts: [...item.contexts],
        visible: item.visible,
        documentUrlPatterns: [...item.documentUrlPatterns],
        ...(item.targetUrlPatterns === undefined ? {} : { targetUrlPatterns: [...item.targetUrlPatterns] }),
      },
      // В Firefox колбэк `create` не зовётся (см. menu-create.ts), поэтому ждём его только в Chrome.
      { lastError: () => browserApi.runtime.lastError, callbackIsReliable: !isFirefox() },
    ),
  setVisible: (id, visible) => browserApi.contextMenus.update(id, { visible }),
};

const askRightClickedVideo: AskRightClickedVideo = async (tabId, frameId) => {
  const results = await browserApi.scripting.executeScript({
    target: { tabId, frameIds: [frameId] },
    func: callIsolatedGlobal,
    args: [RIGHT_CLICKED_VIDEO_FUNCTION_NAME],
  });
  // Пограничник уже проверил ответ MAIN-мира; ответ изолированного мира проверяем ещё раз: он шёл через страницу вкладки.
  return parseRightClickedVideo(results[0]?.result) ?? null;
};

const banner: EntryBanner = {
  // Плашка живёт в верхнем фрейме (`frameId: 0`); страница без нашего скрипта отклонит доставку.
  show: async (tabId, report) => {
    await browserApi.tabs.sendMessage(tabId, { kind: SHOW_ENTRY_RESULT_KIND, report }, { frameId: 0 });
  },
};

const badge: EntryBadge = {
  markProblem: async (tabId) => {
    await browserApi.action.setBadgeText({ tabId, text: "!" });
    await browserApi.action.setBadgeBackgroundColor({ tabId, color: PROBLEM_BADGE_COLOR });
  },
  clearProblem: async (tabId) => {
    // `null` по документации Chrome и Firefox снимает текст ЭТОЙ вкладки, и она снова наследует общий значок
    // статуса связи. Пустая строка вместо `null` перекрыла бы общий «!» пустотой. Типы Chrome `null` не знают.
    await browserApi.action.setBadgeText({ tabId, text: null as unknown as string });
  },
};

const linkTabs: LinkTabsApi = {
  openActiveTab: async (properties) => {
    const tab = await browserApi.tabs.create({ ...properties, active: true });
    if (tab.id === undefined) throw new Error("the browser did not return the new tab id");
    return tab.id;
  },
  activateTab: async (tabId) => void (await browserApi.tabs.update(tabId, { active: true })),
  isTabActive: async (tabId) => (await browserApi.tabs.get(tabId)).active,
  tabExists: (tabId) => browserApi.tabs.get(tabId).then(() => true, () => false),
  closeTab: (tabId) => browserApi.tabs.remove(tabId),
};

const lateTabs: LateTabsApi = {
  reload: (tabId) => browserApi.tabs.reload(tabId),
  // Адрес вкладки доступен по праву на сайты, отдельное право `tabs` не нужно.
  originOf: async (tabId) => {
    try {
      const { url } = await browserApi.tabs.get(tabId);
      return url === undefined ? undefined : new URL(url).origin;
    } catch {
      return undefined;
    }
  },
};

// Фреймы по одному, без ожидания их загрузки и со сроком (сессия 15c): ленивый фрейм рекламы не держит поздний вход.
const executeVideoVisit: ExecuteVideoVisit = (tabId, target) =>
  browserAnswersOfEachFrame(tabId, { func: visitFrameVideos, args: [target, LATE_POSITION_TOLERANCE_S] }, LATE_VIDEO_VISIT_DEADLINE_MS);

/** Репортёр итога входа на настоящем браузере: плашка в верхнем фрейме и вкладочный «!». Им же пользуется режим 2 (12a). */
export function createBrowserEntryReporter(): EntryReporter {
  return createEntryReporter({ banner, badge, note });
}

/** Поставить входы 10c. Вызывается один раз на верхнем уровне фона, после `installTabHandoffs`. */
export function installEntries(handoffs: TabHandoffs, scheduler: Scheduler, reporter: EntryReporter): void {
  const menus = createEntryMenus(menusApi, note);
  const menuEntry = createMenuEntry({
    askRightClickedVideo,
    handOffTab: (tabId, video) => handoffs.handOffTab(tabId, undefined, video),
    reporter,
    note,
  });
  const linkEntry = createLinkEntry({
    tabs: linkTabs,
    handoffs: { surveyTab: (tabId) => handoffs.surveyTab(tabId), handOffTab: (tabId) => handoffs.handOffTab(tabId), activeTabIds: () => handoffs.activeTabIds() },
    scheduler,
    reporter,
    note,
  });

  const lateEntry = createLateEntry({
    tabs: lateTabs,
    videos: createLateVideoReader(executeVideoVisit),
    handoffs: { surveyTab: (tabId) => handoffs.surveyTab(tabId), handOffTab: (tabId) => handoffs.handOffTab(tabId) },
    scheduler,
    reporter,
    note,
  });
  browserApi.runtime.onMessage.addListener(
    createLateEntryCommandListener({ extensionId: browserApi.runtime.id, extensionBaseUrl: browserApi.runtime.getURL("") }, lateEntry),
  );

  // Сбой чтения настроек не молчит и не отключает вход: значения по умолчанию, причина в журнал.
  const readSettings = () => readEntrySettingsOrDefaults(storeOfBrowser, note);

  // Пункты создаются при установке, обновлении и старте браузера (см. шапку entry-menus.ts).
  const installMenus = (): void => {
    readSettings()
      .then((settings) => menus.installAll(settings))
      .catch((error: unknown) => note("could not create the context menu items", error));
  };
  browserApi.runtime.onInstalled.addListener(installMenus);
  browserApi.runtime.onStartup.addListener(installMenus);

  // Выключенный вход исчезает сразу, без перезапуска браузера.
  browserApi.storage.onChanged.addListener((changes, areaName) => {
    if (!isEntrySettingsChange(areaName, changes)) return;
    const settings = entrySettingsFromChange(changes);
    if (settings !== undefined) void menus.applySettings(settings);
  });

  browserApi.contextMenus.onClicked.addListener((click, tab) => {
    readSettings()
      .then((settings) =>
        routeMenuClick(click, tab, settings, {
          playVideo: (tabId, frameId) => menuEntry.playVideo(tabId, frameId),
          playLink: (linkUrl, source) => linkEntry.openLink(linkUrl, source),
        }),
      )
      .catch((error: unknown) => note("context menu click failed", error));
  });

  browserApi.runtime.onMessage.addListener(
    createOverlayEntryListener({
      extensionId: browserApi.runtime.id,
      readSettings,
      handOffTab: (tabId, video) => handoffs.handOffTab(tabId, undefined, video),
      reporter,
      note,
    }),
  );

  browserApi.tabs.onRemoved.addListener((tabId) => linkEntry.onTabRemoved(tabId));
  handoffs.onSessionEnded((tabId, end) => linkEntry.onSessionEnded(tabId, end));
  // Удачная передача из любого входа (в том числе из попапа) снимает вкладочный «!» прошлого отказа.
  handoffs.onSessionStarted((tabId) => void reporter.forgetProblem(tabId));
}
