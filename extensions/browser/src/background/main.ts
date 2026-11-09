// Точка входа фона (service worker в Chrome, background script в Firefox).
// Здесь только сборка частей: поставщик статуса, значок, маршрутизатор порта попапа,
// передача вкладки плееру (src/background/handoff).
import { applyBadge } from "./badge.ts";
import { routeStatusPort } from "./status-port-router.ts";
import { createHostLink } from "./host-link/host-link.ts";
import { createChromeNativePortFactory, createRealScheduler } from "./host-link/native-port.ts";
import type { BridgeStatusProvider } from "./status-provider.ts";
import { startCodecPolicy } from "./codec-policy/browser-wiring.ts";
import { createFirstLabelRequest } from "./codec-policy/first-label-request.ts";
import { installTabModel } from "./tab-model/browser-wiring.ts";
import { installTabHandoffs } from "./handoff/browser-wiring.ts";
import { installChoiceMemory } from "./choice-memory/browser-wiring.ts";
import { createBrowserEntryReporter, installEntries } from "./entries/browser-wiring.ts";
import { installAutoHandoff } from "./auto-handoff/browser-wiring.ts";
import { NATIVE_HOST_NAME } from "../shared/host-name.ts";

// Связь с посредником: единственный владелец порта native messaging (src/background/host-link).
const scheduler = createRealScheduler();

// Политика видеокодеков для сайтов (решение 43): настройка + метка плеера -> регистрация курьера.
const codecPolicy = startCodecPolicy();

const hostLink = createHostLink({
  portFactory: createChromeNativePortFactory(),
  scheduler,
  hostName: NATIVE_HOST_NAME,
  extensionVersion: chrome.runtime.getManifest().version,
  // Метка «дешёвых» кодеков плеера из каждого hello_accepted уходит в кэш политики; первый вопрос
  // о метке (13a) на этом ответе отпускает связь.
  onPlayerCheapVideoCodecs: (families) => {
    void codecPolicy.reportPlayerCheapCodecs(families);
    firstLabelRequest.playerAnswered();
  },
});

// Первый вопрос о метке (решение 266): пока плеер ни разу не отвечал, спросить его сразу, а не на
// первой передаче — иначе первое видео после установки идёт без политики кодеков.
const firstLabelRequest = createFirstLabelRequest({
  knowsPlayerLabel: () => codecPolicy.knowsPlayerLabel(),
  acquireDemand: (reason) => hostLink.acquireDemand(reason),
  scheduler,
});
const statusProvider: BridgeStatusProvider = hostLink;

// Проба «посредник установлен?» — только при старте браузера и установке/обновлении экстеншена.
// Плеер при этом не будится: посредник трогает шину лишь на первом сообщении. Исключение — пока
// метка кодеков неизвестна: тогда вместо пустой пробы идёт настоящий hello (первый вопрос, 13a).
function checkHostOnBrowserEvent(): void {
  hostLink.probeHostPresence();
  void firstLabelRequest.askIfUnknown();
}
chrome.runtime.onStartup.addListener(checkHostOnBrowserEvent);
chrome.runtime.onInstalled.addListener(checkHostOnBrowserEvent);

// Значок следует за статусом: подписка живёт всё время работы фона.
// Начальный статус («unknown») на значок НЕ кладём: браузер хранит значок, пока фон спит,
// и после пробуждения service worker «!» от прошлой проверки должен остаться до новой проверки.
statusProvider.subscribe(applyBadge);

// Каждое подключение порта «bridge-status» (попап) обслуживает роутер; чужие порты он игнорирует.
chrome.runtime.onConnect.addListener((port) => {
  routeStatusPort(port, statusProvider);
});

// Команда «отдать вкладку N» (решение 175) от страниц экстеншена; сеанс держит открытый WebSocket,
// а он продлевает жизнь service worker (Chrome 116+). Состояние сеанса в 09a — в памяти фона.
// Модель вкладки (решение 198): снимки найденных видео в storage.session; попап 10 и реклама 09d
// будут читать её через intent-методы.
const tabModel = installTabModel(scheduler);
// Память выбора озвучки, качества и субтитров (14b2): одно хранилище фона на все сеансы и страницу настроек.
const choiceMemory = installChoiceMemory();
const tabHandoffs = installTabHandoffs(hostLink, scheduler, tabModel, choiceMemory);

// Входы режима 1 (10c): контекстное меню, вход по ссылке, кнопка поверх видео. Все они сводятся к команде
// передачи вкладки `tabHandoffs`; слушатели ставятся здесь же, синхронно, чтобы браузер будил фон по ним.
const entryReporter = createBrowserEntryReporter();
installEntries(tabHandoffs, scheduler, entryReporter);

// Режим 2 «автозапуск» (12a): маршрутизатор донесений сторожа и рекламы; ставится после передачи вкладки и входов.
installAutoHandoff(tabHandoffs, scheduler, entryReporter);
