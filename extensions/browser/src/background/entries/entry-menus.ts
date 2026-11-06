// Пункты контекстного меню (сессия 10c): создание, видимость по настройке, разбор клика.
//
// Три пункта (решения 7, 45, 218):
//  - на `<video>`: «Play in Fastiplayer»;
//  - на странице и во фрейме: «Play video from this page in Fastiplayer» — для сайтов, забирающих правый
//    клик у видео. Виден на любой обычной странице: Chrome не умеет менять меню в момент открытия;
//  - на ссылке: «Play link in Fastiplayer» (только http/https).
// Создаются в `runtime.onInstalled` и `runtime.onStartup` (`removeAll` + `create`). В Chrome пункты переживают
// перезапуск фона; сохраняются ли они в Firefox после перезапуска БРАУЗЕРА, вживую не проверено, а пересоздание
// идемпотентно (сперва `removeAll`) и ничего не стоит, поэтому делаем его и на старте браузера.
// Выключенный в настройках вход скрывается сразу, без перезапуска: `contextMenus.update(visible)`.

import { ENTRY_SETTINGS_KEY, type EntrySettingName, type EntrySettings } from "../../shared/entry-settings.ts";

export const VIDEO_MENU_ID = "fastiplayer-video";
export const PAGE_MENU_ID = "fastiplayer-page";
export const LINK_MENU_ID = "fastiplayer-link";

/** Страницы, где пункты показываются: обычные адреса. Служебные страницы браузера в список не входят. */
export const MENU_DOCUMENT_PATTERNS: readonly string[] = ["http://*/*", "https://*/*", "file:///*"];

/** Ссылки, которые умеем открыть: только http(s) (`file:` и `javascript:` по ссылке не открываем). */
export const LINK_TARGET_PATTERNS: readonly string[] = ["http://*/*", "https://*/*"];

/** Описание пункта: чем он создаётся и какой настройкой включается. */
export interface MenuItemSpec {
  readonly id: string;
  readonly title: string;
  readonly contexts: readonly ("video" | "page" | "frame" | "link")[];
  readonly targetUrlPatterns?: readonly string[];
  /** Настройка, выключающая пункт. */
  readonly setting: EntrySettingName;
}

export const MENU_ITEMS: readonly MenuItemSpec[] = [
  { id: VIDEO_MENU_ID, title: "Play in Fastiplayer", contexts: ["video"], setting: "video_menu" },
  { id: PAGE_MENU_ID, title: "Play video from this page in Fastiplayer", contexts: ["page", "frame"], setting: "video_menu" },
  { id: LINK_MENU_ID, title: "Play link in Fastiplayer", contexts: ["link"], targetUrlPatterns: LINK_TARGET_PATTERNS, setting: "link_menu" },
];

/** Шов к `contextMenus`: каждый вызов — промис, отклоняется при сбое браузера. */
export interface MenusApi {
  removeAll(): Promise<void>;
  create(item: MenuItemSpec & { readonly visible: boolean; readonly documentUrlPatterns: readonly string[] }): Promise<void>;
  setVisible(id: string, visible: boolean): Promise<void>;
}

export interface EntryMenus {
  /**
   * `runtime.onInstalled` и `runtime.onStartup`: пересоздать все пункты с видимостью по текущей настройке. */
  installAll(settings: EntrySettings): Promise<void>;
  /** Настройка поменялась: показать или скрыть пункты. Сбой одного пункта не мешает остальным. */
  applySettings(settings: EntrySettings): Promise<void>;
}

export function createEntryMenus(api: MenusApi, note: (message: string, error: unknown) => void): EntryMenus {
  return {
    async installAll(settings) {
      try {
        await api.removeAll();
      } catch (error) {
        note("could not clear the old context menu items", error);
      }
      // Сбой одного пункта не отменяет остальные (как в `applySettings`).
      for (const item of MENU_ITEMS) {
        try {
          await api.create({ ...item, visible: settings[item.setting], documentUrlPatterns: MENU_DOCUMENT_PATTERNS });
        } catch (error) {
          note(`could not create the menu item ${item.id}`, error);
        }
      }
    },
    async applySettings(settings) {
      for (const item of MENU_ITEMS) {
        try {
          await api.setVisible(item.id, settings[item.setting]);
        } catch (error) {
          note(`could not update the visibility of ${item.id}`, error);
        }
      }
    },
  };
}

// ---------------------------------------------------------------------------------------------
// Разбор клика
// ---------------------------------------------------------------------------------------------

/** Клик по пункту (`OnClickData` в той части, что нужна). */
export interface MenuClick {
  readonly menuItemId: string | number;
  readonly frameId?: number | undefined;
  readonly linkUrl?: string | undefined;
}

/** Вкладка клика (`tabs.Tab` в той части, что нужна). */
export interface ClickedTab {
  readonly id?: number | undefined;
  readonly windowId?: number | undefined;
  readonly index?: number | undefined;
}

/** Куда идёт клик. Каждая ветка — отдельный вход со своим владельцем. */
export interface MenuClickTargets {
  playVideo(tabId: number, frameId: number): Promise<void>;
  playLink(linkUrl: string | undefined, source: { readonly tabId: number; readonly windowId: number | undefined; readonly index: number | undefined }): Promise<void>;
}

/**
 * Распределить клик по входам. Свой пункт выключенного входа сюда не дойдёт (он скрыт), но проверка
 * настройки на клике оставлена: меню в Chrome может показать пункт, скрытый только что.
 */
export async function routeMenuClick(
  click: MenuClick,
  tab: ClickedTab | undefined,
  settings: EntrySettings,
  targets: MenuClickTargets,
): Promise<void> {
  const tabId = tab?.id;
  if (tabId === undefined) return; // клик вне вкладки (не наш случай): некому показать итог
  switch (click.menuItemId) {
    case VIDEO_MENU_ID:
    case PAGE_MENU_ID:
      if (settings.video_menu) await targets.playVideo(tabId, click.frameId ?? 0);
      return;
    case LINK_MENU_ID:
      if (settings.link_menu) await targets.playLink(click.linkUrl, { tabId, windowId: tab?.windowId, index: tab?.index });
      return;
    default:
      return; // чужой пункт (в этом экстеншене других нет)
  }
}

/** `storage.onChanged` нашего ключа? (область `local`, ключ настройки входов). */
export function isEntrySettingsChange(areaName: string, changes: Readonly<Record<string, unknown>>): boolean {
  return areaName === "local" && Object.hasOwn(changes, ENTRY_SETTINGS_KEY);
}
