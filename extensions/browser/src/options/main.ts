// Страница настроек: видео-кодек для сайтов (сессия 09b) и локальные адреса файлов (сессия 09c);
// входы (10c) и режим 2 «автозапуск» (12a); свои правила сайтов — сессия 12b; фильтры видео — решения 260–261;
// память выбора озвучки, качества и субтитров — сессия 14b2.
import { autoHandoffSettingsFromChange, type AutoHandoffSettings } from "../shared/auto-handoff-settings.ts";
import { browserApi } from "../shared/browser-api.ts";
import { mountAutoHandoffSettingControl } from "./auto-handoff-setting-control.ts";
import { mountAutoHandoffListsControl } from "./auto-handoff-lists-control.ts";
import { mountChoiceMemoryControl } from "./choice-memory-control.ts";
import { mountCodecSettingControl } from "./codec-setting-control.ts";
import { mountEntrySettingControl } from "./entry-setting-control.ts";
import { mountLocalAddressSettingControl } from "./local-address-setting-control.ts";
import { mountVideoFiltersControl } from "./video-filters-control.ts";

const container = document.getElementById("options-content");
if (container === null) throw new Error("options.html has no #options-content element");
const localStore = {
  get: (keys: readonly string[]) => browserApi.storage.local.get([...keys]),
  set: (items: Record<string, unknown>) => browserApi.storage.local.set(items),
};
void mountCodecSettingControl(container, localStore);
void mountLocalAddressSettingControl(container, localStore);
void mountEntrySettingControl(container, localStore);
void mountAutoHandoffSettingControl(container, localStore);
/** Подписка разделов режима 2 на новые значения настройки (их пишут и попап, и предложение на видео). */
function onAutoHandoffSettingsChanged(listener: (settings: AutoHandoffSettings) => void): void {
  browserApi.storage.onChanged.addListener((changes, area) => {
    if (area !== "local") return;
    const settings = autoHandoffSettingsFromChange(changes);
    if (settings !== undefined) listener(settings);
  });
}
void mountVideoFiltersControl(container, { store: localStore, onSettingsChanged: onAutoHandoffSettingsChanged });
void mountAutoHandoffListsControl(container, { store: localStore, onSettingsChanged: onAutoHandoffSettingsChanged });
// Память выбора принадлежит фону: число записей и «Forget» — сообщением ему, объём — обычная настройка.
void mountChoiceMemoryControl(container, { store: localStore, askBackground: (request) => browserApi.runtime.sendMessage(request) });
