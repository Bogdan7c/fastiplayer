// Страница настроек. Пока одна настройка — видео-кодек для сайтов (сессия 09b); режимы и
// переключатели придут в сессиях 10/12.
import { browserApi } from "../shared/browser-api.ts";
import { mountCodecSettingControl } from "./codec-setting-control.ts";

const container = document.getElementById("options-content");
if (container === null) throw new Error("options.html has no #options-content element");
void mountCodecSettingControl(container, {
  get: (keys) => browserApi.storage.local.get([...keys]),
  set: (items) => browserApi.storage.local.set(items),
});
