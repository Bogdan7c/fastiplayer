// Изолированный контент-скрипт (ISOLATED world), document_start, все фреймы.
// Здесь только сборка: личный канал с MAIN-миром, пограничник, порт фона, функция выбора видео.
//
// До передачи скрипт ничего не шлёт фону и не будит его: порт участка 2 открывает сам фон
// (tabs.connect) в момент «отдать вкладку», а описание видео фон запрашивает через executeScript.
import { browserApi } from "../shared/browser-api.ts";
import { ARM_NEXT_VIDEO_ACCEPTED, HANDOFF_PORT_NAME, type HeldVideoReply } from "../shared/handoff-contract.ts";
import { parseArmNextVideoRequest, parseGrantPlayCreditRequest, parseReleaseHeldVideoRequest } from "../shared/page-message-validation.ts";
import { parseWithdrawViewerOfferRequest } from "../shared/viewer-offer-contract.ts";
import { RIGHT_CLICKED_VIDEO_FUNCTION_NAME } from "../shared/entry-contract.ts";
import { VIDEO_CANDIDATES_FUNCTION_NAME, type ScriptedFrameAnswer } from "../shared/video-candidates-probe.ts";
import { installAdWatch } from "./ad-watch-entry.ts";
import { createAdapterHandClient } from "./adapter-hand-client.ts";
import { installAutoHandoffWatch } from "./auto-handoff-watch-entry.ts";
import { listenForHandedOffVideo } from "./handed-off-video-listener.ts";
import { installPageUi } from "./page-ui/page-ui-entry.ts";
import { openPageChannel, type PageChannelPort } from "./page-channel.ts";
import { createContentRelay, type ContentRelay } from "./relay.ts";
import { listenForViewerOffer } from "./viewer-offer-listener.ts";
import { installVariantWatch } from "./variant-watch-entry.ts";

/** Сериализация непрозрачного происхождения (`about:blank`, `data:`). */
const OPAQUE_ORIGIN = "null";

/** Версия этого скрипта: читается один раз на старте, пока экстеншен точно жив. */
const SCRIPT_VERSION = browserApi.runtime.getManifest().version;

let debugReported = false;

// Пограничник создаётся до канала (канал сразу передаёт ему сообщения), отправка — через ссылку.
let sendToPageWorld: (message: unknown) => void = () => {};
const relay: ContentRelay = createContentRelay({
  sendToPage: (message) => sendToPageWorld(message),
  reportHeldVideo: (report) => browserApi.runtime.sendMessage(report) as Promise<HeldVideoReply>,
  // Выбор человека на предложении «Ask» (12b); ответ проверяет пограничник.
  sendViewerOfferChoice: (message) => Promise.resolve(browserApi.runtime.sendMessage(message)),
  timers: {
    setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
    clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
  },
  // Журнала в продукте нет: одна строка за жизнь документа, только для разработчика в консоли.
  debugOnce: (message) => {
    if (debugReported) return;
    debugReported = true;
    console.debug(message);
  },
  now: () => performance.now(),
  // Читается в изолированном мире: страница не может подменить то, что документ говорит о себе.
  ownDocument: () => ({
    hostname: location.hostname,
    origin: location.origin === OPAQUE_ORIGIN ? null : location.origin,
  }),
});

// Клиент руки-функции адаптеров (14b): свои вызовы MAIN-миру и свой пограничник ответов.
const adapterHand = createAdapterHandClient({
  sendToPage: (call) => sendToPageWorld(call),
  timers: {
    setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
    clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
  },
  now: () => performance.now(),
});

const channel = openPageChannel(
  {
    createChannel: () => {
      const messageChannel = new MessageChannel();
      return { port1: messageChannel.port1 as PageChannelPort, port2: messageChannel.port2 };
    },
    postToOwnWindow: (message, transfer) => window.postMessage(message, "*", transfer),
    rendezvous: window,
    createCancelableEvent: (type) => new Event(type, { cancelable: true }),
  },
  // Ответы руки забирает её клиент; всё остальное — пограничник передачи.
  (message) => {
    if (!adapterHand.acceptPageMessage(message)) relay.handlePageMessage(message);
  },
);
sendToPageWorld = (message) => channel.send(message);

// Порт участка 2 открывает только наш фон; чужие экстеншены к нашим контент-скриптам не подключаются,
// но имя и отправителя всё равно проверяем.
browserApi.runtime.onConnect.addListener((port) => {
  const senderId = port.sender?.id;
  if (port.name !== HANDOFF_PORT_NAME || (senderId !== undefined && senderId !== browserApi.runtime.id)) return;
  relay.acceptBackgroundPort(port);
});

// Взвод сторожа следующего видео: фон зовёт все фреймы вкладки сразу. Только наш фон: у него нет `tab`.
// Ответ «взведён» нужен возврату страницы (11b2): по нему фон знает, что наш скрипт в новой вкладке уже есть.
browserApi.runtime.onMessage.addListener((message: unknown, sender, sendResponse) => {
  if (sender.id !== browserApi.runtime.id || sender.tab !== undefined) return;
  const request = parseArmNextVideoRequest(message);
  if (request !== undefined) {
    relay.armNextVideo(request.lease_ms, request.page_return);
    sendResponse(ARM_NEXT_VIDEO_ACCEPTED);
    return;
  }
  // Режим 2 (12a): «кредит клика» всем фреймам и «отпусти задержанное» (ключ элемента знает только его фрейм).
  // Ответа не нужно: фон не ждёт, а чужой ключ во фрейме — это просто «такого элемента здесь нет».
  const creditRequest = parseGrantPlayCreditRequest(message);
  if (creditRequest !== undefined) {
    relay.grantPlayCredit(creditRequest.lease_ms);
    return;
  }
  const releaseRequest = parseReleaseHeldVideoRequest(message);
  if (releaseRequest !== undefined) {
    relay.releaseHeldVideo(releaseRequest.element);
    return;
  }
  // Предложение «Ask» (12b): на вкладке встало новое — прежнее снимается, его видео стоит на паузе.
  const withdrawRequest = parseWithdrawViewerOfferRequest(message);
  if (withdrawRequest !== undefined) relay.withdrawViewerOffer(withdrawRequest.element);
});

// Охрана режима 2 (12a): читает настройку и говорит MAIN-миру «включена/выключена». Фону не пишет ничего.
installAutoHandoffWatch(relay);

// Наблюдение за рекламой (сессия 09d): спит, пока фон не возьмёт его в аренду.
installAdWatch();

// Наблюдение за вариантами страницы (сессия 14b): тоже спит до аренды от фона.
installVariantWatch(adapterHand);

// Функция для executeScript фона живёт в глобале ИЗОЛИРОВАННОГО мира: странице она не видна.
Object.defineProperty(globalThis, VIDEO_CANDIDATES_FUNCTION_NAME, {
  value: async (): Promise<ScriptedFrameAnswer> => ({ script_version: SCRIPT_VERSION, candidates: await relay.describeVideoCandidates() }),
  configurable: false,
  enumerable: false,
  writable: false,
});

// Видео под последним настоящим правым кликом (10c): фон зовёт её через executeScript в нужном фрейме.
// Глобал изолированного мира, как и функция выше: странице не виден.
Object.defineProperty(globalThis, RIGHT_CLICKED_VIDEO_FUNCTION_NAME, {
  value: () => relay.rightClickedVideo(),
  configurable: false,
  enumerable: false,
  writable: false,
});

// Плашка с итогом входа и кнопка поверх видео (10c). Кнопка спит, пока настройка выключена; фону не пишет,
// пока человек не нажал её.
const pageUiSinks = installPageUi({
  videoAtPoint: (x, y) => relay.videoAtPoint(x, y),
  requestViewerReturn: () => relay.requestViewerReturn(),
  answerViewerOffer: (choice) => relay.answerViewerOffer(choice),
});
const addAnchorCaptureListener = (eventName: string, listener: (event: Event) => void): void => window.addEventListener(eventName, listener, { capture: true });

// Заглушка над переданным видео (11a, решение 236): слушатель якоря встаёт ДО того, как его имя уйдёт MAIN-миру
// по личному порту (до встречи миров порт копит сообщение). Фону это имя не уходит никогда.
const handedOffVideoListener = listenForHandedOffVideo(
  {
    addCaptureListener: addAnchorCaptureListener,
    // Генератор ЭТОГО мира: страница подменить его не может.
    randomFill: (bytes) => void crypto.getRandomValues(bytes),
  },
  pageUiSinks.handedOffVideo,
);
// Панель предложения «Ask» (12b): своё событие, производное от того же тайного имени. Тоже ДО отправки имени.
listenForViewerOffer(addAnchorCaptureListener, handedOffVideoListener.watchMessage.event_name, pageUiSinks.viewerOffer);
channel.send(handedOffVideoListener.watchMessage);
