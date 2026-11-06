// Страница настроек: видео-кодек для сайтов (сессия 09b) и локальные адреса файлов (сессия 09c);
// режимы и переключатели придут в сессиях 10/12.
import { browserApi } from "../shared/browser-api.ts";
import { mountCodecSettingControl } from "./codec-setting-control.ts";
import { mountEntrySettingControl } from "./entry-setting-control.ts";
import { mountLocalAddressSettingControl } from "./local-address-setting-control.ts";

const container = document.getElementById("options-content");
if (container === null) throw new Error("options.html has no #options-content element");
const localStore = {
  get: (keys: readonly string[]) => browserApi.storage.local.get([...keys]),
  set: (items: Record<string, unknown>) => browserApi.storage.local.set(items),
};
void mountCodecSettingControl(container, localStore);
void mountLocalAddressSettingControl(container, localStore);
void mountEntrySettingControl(container, localStore);
