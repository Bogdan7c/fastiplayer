// Сборка слежения за настройкой режима 2 на настоящих API браузера (изолированный мир, все фреймы).
// Логика — в `auto-handoff-watch.ts`; здесь только переходники. Фону этот модуль не пишет ничего.
import { browserApi } from "../shared/browser-api.ts";
import { readAutoHandoffSettingsOrDefaults } from "../shared/auto-handoff-settings.ts";
import { watchAutoHandoffSettings } from "./auto-handoff-watch.ts";
import type { ContentRelay } from "./relay.ts";

/** Строка для разработчика в консоли: журнала в продукте нет. */
function noteForDeveloper(message: string, error: unknown): void {
  console.debug(`[fastiplayer] ${message}:`, error);
}

/** Подключить слежение к этому документу; зовётся один раз при загрузке контент-скрипта. */
export function installAutoHandoffWatch(relay: Pick<ContentRelay, "setAutoHandoffWatch">): void {
  watchAutoHandoffSettings({
    readSettings: () => readAutoHandoffSettingsOrDefaults(browserApi.storage.local, noteForDeveloper),
    onLocalStorageChanged: (listener) => {
      const handler = (changes: Record<string, chrome.storage.StorageChange>, areaName: string): void => {
        if (areaName === "local") listener(changes);
      };
      browserApi.storage.onChanged.addListener(handler);
      return () => browserApi.storage.onChanged.removeListener(handler);
    },
    applyWatch: (state) => relay.setAutoHandoffWatch(state),
    note: noteForDeveloper,
  });
}
