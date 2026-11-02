// Точка входа фона (service worker в Chrome, background script в Firefox).
// Здесь только сборка частей: поставщик статуса, значок, маршрутизатор порта попапа.
import { applyBadge } from "./badge.ts";
import { routeStatusPort } from "./status-port-router.ts";
import { createHostLink } from "./host-link/host-link.ts";
import { createChromeNativePortFactory, createRealScheduler } from "./host-link/native-port.ts";
import type { BridgeStatusProvider } from "./status-provider.ts";
import { NATIVE_HOST_NAME } from "../shared/host-name.ts";

// Связь с посредником: единственный владелец порта native messaging (src/background/host-link).
const hostLink = createHostLink({
  portFactory: createChromeNativePortFactory(),
  scheduler: createRealScheduler(),
  hostName: NATIVE_HOST_NAME,
  extensionVersion: chrome.runtime.getManifest().version,
});
const statusProvider: BridgeStatusProvider = hostLink;

// Проба «посредник установлен?» — только при старте браузера и установке/обновлении экстеншена.
// Плеер при этом не будится: посредник трогает шину лишь на первом сообщении.
chrome.runtime.onStartup.addListener(() => hostLink.probeHostPresence());
chrome.runtime.onInstalled.addListener(() => hostLink.probeHostPresence());

// Значок следует за статусом: подписка живёт всё время работы фона.
// Начальный статус («unknown») на значок НЕ кладём: браузер хранит значок, пока фон спит,
// и после пробуждения service worker «!» от прошлой проверки должен остаться до новой проверки.
statusProvider.subscribe(applyBadge);

// Каждое подключение порта «bridge-status» (попап) обслуживает роутер; чужие порты он игнорирует.
chrome.runtime.onConnect.addListener((port) => {
  routeStatusPort(port, statusProvider);
});
