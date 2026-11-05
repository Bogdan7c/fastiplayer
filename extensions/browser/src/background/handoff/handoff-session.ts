// Одна передача видео вкладки плееру: от `open_session` до снятия тени (сессия 09a).
//
// Порядок старта (сначала плеер, потом страница): нужда связи → `open_session` → WebSocket и
// `attached` → порт участка 2 к выбранному фрейму → `begin_handoff` → `handoff_began{started}`.
// Так отказ плеера (самый частый сбой) вообще не трогает страницу: тень ставится только тогда,
// когда плеер уже принял сеанс.
//
// Дальше сеанс связывает три стороны:
// - события страницы → переводчик (поколения, рамки) → очередь сокета;
// - сообщения плеера → команды тени странице (`clock`/`play`/`pause`/`set_rate`/`seek`);
// - конец: `return_to_page` и отказ текущего поколения снимают тень (`end_handoff`), обрыв сокета —
//   тоже (позиция тени, «как тень»); уход со страницы (порт закрылся или команда в него не ушла —
//   Д-7) и закрытие вкладки шлют плееру `tab_gone`.
// Честные концы со стороны страницы, вкладка на месте (сессия 09a2) — без `tab_gone`, очередь
// страницы сначала дописывается плееру:
// - `handoff_ended{element_released}` — страница забрала видео → код 4009 (решение 179);
// - `handoff_ended{returned}` без нашего `end_handoff` — сбой внутри страницы → 4003 (Д-6);
// - тень молчит дольше предела пульса → снять «как тень» и 4003 (решение 180, Д-2);
// - страница превысила бюджет событий (донесение изолированного мира `page_flood`, решение 200) или
//   назвала адрес файла, который правило запрещает (решение 199) → снять «как тень» и 4003.
// Конец сообщается владельцу ровно один раз (`onFinished`), нужда связи освобождается ровно один раз.
//
// Переезд на следующее видео (сессия 09c2, решения 201, 204): реклама кончилась или её пропустили, а
// сеанс живёт. Пока переезд открыт (`video-relocation.ts`), страница, убравшая видео, не значит «забрала
// видео»: фаза `between_videos`, окно держит последний кадр. Сторож задержал следующее видео — фаза
// `moving`: прежняя тень снимается, новая принимается той же командой `begin_handoff`. Другой фрейм —
// новый порт, номера поколений общие. Предел ожидания вышел — честный конец с кодом 4009.

import type {
  HandOffTabOutcome,
  HandoffEndCause,
  HandoffStartOutcome,
  HeldVideoReply,
  HeldVideoReport,
  PageCommand,
  PageEvent,
} from "../../shared/handoff-contract.ts";
import { parsePageEvent, parsePageFloodNotice } from "../../shared/page-message-validation.ts";
import {
  SESSION_CLOSE_CODES,
  type PageIdentity,
  type PlayerSessionMessage,
  type TabGoneReason,
} from "../../protocol/index.ts";
import type { OpenSessionOutcome, OpenSessionRequest } from "../host-link/open-session-requests.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { createTimerSlot } from "../host-link/timer-slot.ts";
import { HANDOFF_BEGIN_REPLY_TIMEOUT_MS, HANDOFF_END_REPLY_TIMEOUT_MS } from "./constants.ts";
import type { FileReferrer } from "../../shared/referrer-policy.ts";
import { ruleAllowsFileUrl, type FileAddressRule } from "./file-address-guard.ts";
import type { FileReferrerRules } from "./file-referrer-rule.ts";
import type { LocalAddressBlock } from "./local-address-block-rules.ts";
import type { FrameConnector, FramePort } from "./frame-port.ts";
import { createDocumentTranslator, type DocumentTranslator, type OutgoingItem, type PageStreamProblem, type StreamPageEvent } from "./page-event-translator.ts";
import { createSessionAdMirror, type AdMirrorBrowser } from "./ad-skip-mirror.ts";
import type { HandoffSession, HandoffSessionEnd } from "./session-contract.ts";

export type { HandoffSession, HandoffSessionEnd } from "./session-contract.ts";
import { translatePlayerMessage } from "./player-message-translator.ts";
import {
  NORMAL_CLOSE_CODE,
  openSessionSocket,
  type QueueDeliveringCloseReason,
  type SessionSocket,
  type SocketEndCause,
  type WebSocketFactory,
} from "./session-socket.ts";
import { createRangeFetcher, type RangeFetch, type RangeFetcher } from "./range-fetcher.ts";
import { judgeHeldVideo } from "./held-video-choice.ts";
import { createShadowSilenceWatch } from "./shadow-silence-watch.ts";
import { createStreamNumbering, type StreamNumbering } from "./stream-numbering.ts";
import type { ChosenVideo } from "./video-choice.ts";
import { createVideoRelocation } from "./video-relocation.ts";
import { describeRefusal, describeSocketEnd } from "./session-outcome-words.ts";
import { disconnectQuietly, END_WITHOUT_PLAYER, LEAVE_PREVIOUS_VIDEO } from "./session-page-commands.ts";

/** Элемент очереди открывает новое поколение (`generation_started`). */
function startsGeneration(item: OutgoingItem): boolean {
  return item.kind === "message" && item.message.type === "generation_started";
}

/** Что сеансу нужно от связи с посредником (HostLink). */
export interface SessionDoor {
  openSession(request: OpenSessionRequest): Promise<OpenSessionOutcome>;
  acquireDemand(reason: string): () => void;
}

export interface HandoffSessionDependencies {
  readonly door: SessionDoor;
  readonly createWebSocket: WebSocketFactory;
  readonly scheduler: Scheduler;
  readonly connectFrame: FrameConnector;
  /** Жива ли вкладка (отличить «закрыли вкладку» от «ушли со страницы»). */
  readonly tabExists: (tabId: number) => Promise<boolean>;
  /** `tabs.get`: заголовок и адрес; `undefined` — вкладки нет. */
  readonly describeTab: (tabId: number) => Promise<TabDescription | undefined>;
  /** Сеть браузера для байтов файла `<video src>` (сессия 09b; в продукте — `fetch` фона). */
  readonly fetchRange: RangeFetch;
  /** Referer закачки файла (решение 202): правило браузера на время одного запроса. */
  readonly fileReferrer: FileReferrerRules;
  /** Сетевой заслон локальных адресов (решение 199): разрешение сеанса на время одного запроса. */
  readonly localAddressBlock: LocalAddressBlock;
  /** Взвести сторож следующего видео всем фреймам вкладки (сессия 09c2). */
  readonly armNextVideo: (tabId: number, leaseMs: number) => void;
  /** Браузерная сторона дублёра кнопки пропуска рекламы: аренда наблюдения, нажатие, вывод вкладки (09d). */
  readonly adMirrorBrowser: AdMirrorBrowser;
  /** Отметка в журнал разработчика (устаревшая команда плеера, сбой вывода вкладки). */
  readonly noteForDeveloper: (message: string) => void;
}

/** Что фон знает о вкладке: подпись окна и адрес документа. */
export interface TabDescription {
  readonly title?: string | undefined;
  readonly url?: string | undefined;
}

/** Что и куда передаём. */
export interface HandoffTarget {
  readonly tabId: number;
  /** Адрес вкладки при старте: его смена значит, что человек ушёл со страницы. */
  readonly tabUrl: string | undefined;
  readonly frameId: number;
  /** Метка документа, из которого выбран элемент (договор: `begin_handoff.document`). */
  readonly document: string;
  readonly element: number;
  readonly sessionKey: string;
  readonly page: PageIdentity;
  /** Номер запроса `begin_handoff` (уникален в фоне). */
  readonly beginRequestId: number;
  /** Правило адреса файла (решение 199): `file_source` и перенаправления проверяются им же. */
  readonly fileAddressRule: FileAddressRule;
  /** Что передаётся: реклама или фильм, и может ли «фильм» оказаться рекламой (сессия 09c2). */
  readonly video: Pick<ChosenVideo, "role" | "mayBeAd">;
  /** Новый номер запроса `begin_handoff` для переезда. */
  readonly nextBeginRequestId: () => number;
}

/**
 * Фазы:
 * - opening — ждём `open_session` и `attached`;
 * - abandoning — сеанс выдан, но страница пропала: ждём `attached`, чтобы честно послать `tab_gone`;
 * - beginning — порт к фрейму открыт, ждём `handoff_began`;
 * - active — тень стоит, поток идёт, сторож тишины слушает пульс тени;
 * - between_videos — тени нет, страница убрала видео, а следующее ещё не появилось (сессия 09c2);
 * - moving — фон принял задержанное видео и ждёт `handoff_began` от его документа;
 * - ending — мы послали `end_handoff`, ждём `handoff_ended{returned}` (или таймаут);
 * - finished — всё освобождено.
 */
type SessionPhase = "opening" | "abandoning" | "beginning" | "active" | "between_videos" | "moving" | "ending" | "finished";

/** Где сейчас стоит сеанс. Меняется при переезде. */
interface SessionPlace {
  readonly frameId: number;
  readonly beginRequestId: number;
  readonly fileAddressRule: FileAddressRule;
}

/** Главный документ вкладки: его порт умирает только вместе со страницей, а не с рекламой. */
const TOP_FRAME_ID = 0;

/** Ушла ли команда в порт страницы. */
type PageDelivery = "delivered" | "port_dead";

/** Подсказки для окна до первых байтов: в 09a длительность и позицию узнаём только из потока. */
const UNKNOWN_HINTS: OpenSessionRequest["hints"] = { duration: { kind: "unknown" }, position: { kind: "unknown" } };

/** Причина нужды связи (для отладки HostLink). */
const DEMAND_REASON = "handoff";

export function startHandoffSession(
  target: HandoffTarget,
  dependencies: HandoffSessionDependencies,
  onFinished: (end: HandoffSessionEnd) => void,
): HandoffSession {
  const beginTimer = createTimerSlot(dependencies.scheduler);
  const endTimer = createTimerSlot(dependencies.scheduler);
  const numbering: StreamNumbering = createStreamNumbering();
  // Слушает пульс тени только в фазе `active`; в `ending` у нас свой предел ответа (endTimer).
  const silenceWatch = createShadowSilenceWatch(dependencies.scheduler, () => endForSilentShadow());
  const relocation = createVideoRelocation({
    scheduler: dependencies.scheduler,
    armFrames: (leaseMs) => dependencies.armNextVideo(target.tabId, leaseMs),
    onWaitExpired: () => endForNoNextVideo(),
  });
  // Дублёр кнопки пропуска рекламы (09d): аренда наблюдения во фреймах, сводка `ad_state`, команды плеера.
  const adMirror = createSessionAdMirror({
    tabId: target.tabId,
    scheduler: dependencies.scheduler,
    browser: dependencies.adMirrorBrowser,
    sessionSaysAdOnScreen: () => relocation.adOnScreen(),
    sendAdState: (ad) => socket?.enqueue({ kind: "message", message: { type: "ad_state", ad } }),
    note: dependencies.noteForDeveloper,
  });
  const releaseDemand = dependencies.door.acquireDemand(DEMAND_REASON);
  let phase: SessionPhase = "opening";
  let place: SessionPlace = { frameId: target.frameId, beginRequestId: target.beginRequestId, fileAddressRule: target.fileAddressRule };
  // Видео, на которое идёт тень сейчас: его признаки войдут в силу с `handoff_began{started}`.
  let incomingVideo: Pick<ChosenVideo, "role" | "mayBeAd"> = target.video;
  let tabRemoved = false;
  let socket: SessionSocket | undefined;
  let framePort: FramePort | undefined;
  // Фрейм живого порта.
  let portFrameId = target.frameId;
  let translator: DocumentTranslator | undefined;
  // Загрузчик диапазонов поколения-файла (сессия 09b); у MSE его нет.
  let fileFeed: RangeFetcher | undefined;
  // Чем кончится передача, если страница подтвердит снятие тени (запоминается при `end_handoff`).
  let pendingEnd: HandoffSessionEnd | undefined;
  // Плеер ответил `attached` на наш `attach`.
  let socketAttached = false;
  // Порт страницы признан мёртвым, ждём ответа «жива ли вкладка»: повторные потери команд
  // за это время (плеер шлёт часы каждые 250 мс) второй вопрос не задают.
  let askingWhetherTabExists = false;
  // Сеанс выдан, но страница для него пропала: чем ответить и какой `tab_gone` послать после attach.
  let abandoned: { readonly outcome: HandOffTabOutcome; readonly reason: TabGoneReason } | undefined;
  let resolveOutcome: (outcome: HandOffTabOutcome) => void = () => {};
  const outcome = new Promise<HandOffTabOutcome>((resolve) => (resolveOutcome = resolve));

  // ---------------------------------------------------------------- конец

  function finish(end: HandoffSessionEnd): void {
    if (phase === "finished") return;
    phase = "finished";
    beginTimer.cancel();
    endTimer.cancel();
    silenceWatch.stop();
    relocation.stop();
    adMirror.stop();
    // Конец сеанса: текущий запрос файла обрывается, ни рамки после конца.
    disposeFileFeed();
    if (framePort !== undefined) disconnectQuietly(framePort);
    releaseDemand();
    // Если старт не успел завершиться, исход объясняет, почему.
    resolveOutcome(end.kind === "start_failed" ? end.outcome : { kind: "failed", reason: `ended_before_start:${end.kind}` });
    onFinished(end);
  }

  function failStart(startOutcome: HandOffTabOutcome, closeCode: number = NORMAL_CLOSE_CODE): void {
    socket?.close(closeCode);
    finish({ kind: "start_failed", outcome: startOutcome });
  }

  /**
   * Плеер уже выдал сеанс, но страница для него пропала (вкладку закрыли, ушли со страницы,
   * элемента больше нет). Плеер не должен ждать «сироту»: без `attach` он держал бы выданный
   * пропуск до своего таймаута первого attach (15 с), а голое закрытие сокета он понял бы как
   * обрыв связи с браузером. Поэтому по протоколу: `attach` → `tab_gone{reason}` → закрытие 4002 —
   * плеер сразу узнаёт настоящую причину и закрывает сеанс. Нового сообщения протокола не нужно.
   * Исход команды сообщается сразу; сам сеанс кончается, когда `tab_gone` ушёл (или сокет умер).
   */
  function abandonGrantedSession(startOutcome: HandOffTabOutcome, reason: TabGoneReason): void {
    resolveOutcome(startOutcome);
    if (socket !== undefined && socketAttached) {
      socket.sendTabGoneAndClose(reason);
      finish({ kind: "start_failed", outcome: startOutcome });
      return;
    }
    // Сокет ещё не дошёл до `attached`: ждём его (предел — таймаут attach в session-socket.ts).
    phase = "abandoning";
    abandoned = { outcome: startOutcome, reason };
    beginTimer.cancel();
    if (framePort !== undefined) disconnectQuietly(framePort);
    framePort = undefined;
  }

  /** Снять тень и ждать подтверждения страницы; сокет закрывается по подтверждению или таймауту. */
  function endHandoff(command: Extract<PageCommand, { kind: "end_handoff" }>, end: HandoffSessionEnd): void {
    if (phase !== "active") return;
    phase = "ending";
    pendingEnd = end;
    // Тень снимаем мы: дальше ждём только её подтверждения, пульс больше не нужен.
    silenceWatch.stop();
    // Предел ожидания ставится ДО отправки: страница, принявшая команду, может так и не ответить.
    // Отправка, которая не ушла (порт мёртв), кончает сеанс сразу — путём ухода страницы (Д-7).
    endTimer.start(HANDOFF_END_REPLY_TIMEOUT_MS, () => completeEnding());
    commandPage(command);
  }

  /**
   * Передача кончилась по стороне страницы, а вкладка на месте: плеер получает всё, что страница
   * успела прислать, и закрытие с причиной — без `tab_gone` (он значил бы «вкладка ушла», и плеер
   * стал бы доигрывать скачанное при живой странице).
   */
  function endOnPageSide(closeReason: QueueDeliveringCloseReason, end: HandoffSessionEnd): void {
    socket?.closeAfterDeliveringQueue(closeReason);
    finish(end);
  }

  /**
   * Тень молчит дольше `SHADOW_SILENCE_LIMIT_MS` (решение 180): потерялся `handoff_ended`, страница
   * зависла или сломан канал. Странице — снять тень «как тень» (best effort: если порт мёртв,
   * его закрытие в `finish` снимет тень в изолированном мире само), плееру — 4003 «Stream error —
   * video returned to the page».
   */
  function endForSilentShadow(): void {
    endForPageFault({ kind: "shadow_silent" });
  }

  /**
   * Вина страницы, ждать её согласия незачем: странице — снять тень «как тень» (если порт мёртв, его
   * закрытие в `finish` снимет тень в изолированном мире само), плееру — всё, что уже в очереди, и 4003.
   */
  function endForPageFault(end: HandoffSessionEnd): void {
    if (phase !== "active" && phase !== "between_videos" && phase !== "moving") return;
    // Результат отправки здесь намеренно не важен: сеанс кончается в любом случае, а путь «порт
    // мёртв» (Д-7) лишь подменил бы честную причину конца на «ушли со страницы».
    tryPostToPage(END_WITHOUT_PLAYER);
    endOnPageSide("protocol_violation", end);
  }

  /**
   * Тень снята по нашему `end_handoff` (или страница не ответила вовремя) — закрыть сокет.
   * Возврат по просьбе плеера (`return_to_page`) — обычное 1000: конец решил и объяснил сам плеер.
   * Отказ текущего поколения (плеер не смог играть, решение владельца 09a2) — 4003 «Stream error —
   * video returned to the page»: иначе окно гасло бы молча, а видео уже играет страница.
   */
  function completeEnding(): void {
    const end = pendingEnd ?? { kind: "returned_to_page" };
    if (end.kind === "stream_rejected") {
      socket?.closeAfterDeliveringQueue("protocol_violation");
    } else {
      socket?.close(NORMAL_CLOSE_CODE);
    }
    finish(end);
  }

  // ---------------------------------------------------------------- старт

  async function openPlayerSession(): Promise<void> {
    const granted = await dependencies.door.openSession({
      session_key: target.sessionKey,
      page: target.page,
      intent: "window",
      hints: UNKNOWN_HINTS,
    });
    if (phase !== "opening") return;
    if (granted.kind !== "granted") {
      failStart(describeRefusal(granted));
      return;
    }
    socket = openSessionSocket({
      port: granted.grant.port,
      sessionKey: target.sessionKey,
      token: granted.grant.token,
      createWebSocket: dependencies.createWebSocket,
      scheduler: dependencies.scheduler,
      callbacks: { onAttached: handleAttached, onPlayerMessage: handlePlayerMessage, onEnded: handleSocketEnded },
    });
    // Вкладку закрыли, пока плеер выдавал сеанс: сеанс выдан — честно закрываем его через attach.
    if (tabRemoved) abandonGrantedSession({ kind: "failed", reason: "tab_closed" }, "closed");
  }

  function handleAttached(): void {
    socketAttached = true;
    if (phase === "abandoning" && abandoned !== undefined) {
      socket?.sendTabGoneAndClose(abandoned.reason);
      finish({ kind: "start_failed", outcome: abandoned.outcome });
      return;
    }
    beginOnPage();
  }

  /** Открыть порт к фрейму и начать с ним новый документ. Бросает, если браузер не даёт порт. */
  function connectToFrame(frameId: number): void {
    const port = dependencies.connectFrame(target.tabId, frameId);
    framePort = port;
    // Новое подключение = новый документ: свои локальные ключи MediaSource, общий счёт поколений.
    translator = createDocumentTranslator(numbering);
    port.onMessage.addListener((message) => handleFrameMessage(port, message));
    port.onDisconnect.addListener(() => handleFrameGone(port));
  }

  function beginOnPage(): void {
    if (phase !== "opening") return;
    try {
      connectToFrame(target.frameId);
    } catch (error) {
      abandonGrantedSession({ kind: "failed", reason: `frame_connect_failed:${String(error)}` }, "navigated_away");
      return;
    }
    phase = "beginning";
    beginTimer.start(HANDOFF_BEGIN_REPLY_TIMEOUT_MS, () =>
      abandonGrantedSession({ kind: "failed", reason: "page_did_not_answer" }, "navigated_away"),
    );
    commandPage({ kind: "begin_handoff", request_id: target.beginRequestId, document: target.document, element: target.element });
  }

  // ---------------------------------------------------------------- переезд на следующее видео

  /**
   * Страница убрала видео, а переезд открыт: тени больше нет, окно держит последний кадр. Плееру — пауза:
   * реклама, оборванная посреди ролика, не должна доигрывать звук, пока следующий ролик грузится.
   */
  function waitForNextVideo(): void {
    phase = "between_videos";
    silenceWatch.stop();
    beginTimer.cancel();
    relocation.videoLeft();
    forwardStreamEvent({ kind: "page_playback", request: { kind: "pause" } });
  }

  /** Следующего видео нет дольше предела: страница видео не поставила, честный конец (решение 179). */
  function endForNoNextVideo(): void {
    if (phase === "active") tryPostToPage(END_WITHOUT_PLAYER);
    if (phase === "active" || phase === "between_videos") endOnPageSide("page_took_video_back", { kind: "page_took_video_back" });
  }

  function heldVideo(frameId: number, report: HeldVideoReport): HeldVideoReply {
    if ((phase !== "active" && phase !== "between_videos") || !relocation.adoptionOpen()) return "release";
    const decision = judgeHeldVideo(frameId, report, place.fileAddressRule);
    if (decision.kind === "release") return "release";
    moveToVideo(decision.video, decision.fileAddressRule);
    return "adopt";
  }

  /**
   * Переезд. Прежняя тень снимается на начало ролика и на паузе: конец файла дал бы странице второе
   * `ended`. Её `handoff_ended` в фазе `moving` ничего не значит. Тот же документ — тот же порт и те же
   * локальные ключи MediaSource; другой фрейм — новый порт, старый закрывается следом за командой.
   */
  function moveToVideo(video: ChosenVideo, fileAddressRule: FileAddressRule): void {
    const leavingPort = framePort;
    if (phase === "active") tryPostToPage(LEAVE_PREVIOUS_VIDEO);
    silenceWatch.stop();
    phase = "moving";
    incomingVideo = video;
    place = { frameId: video.frameId, beginRequestId: target.nextBeginRequestId(), fileAddressRule };
    beginTimer.start(HANDOFF_BEGIN_REPLY_TIMEOUT_MS, () => {
      // Страница ответит позже, а её ответ фаза уже не примет: тень не должна остаться без хозяина.
      tryPostToPage(END_WITHOUT_PLAYER);
      waitForNextVideo();
    });
    if (leavingPort === undefined || video.frameId !== portFrameId) {
      try {
        connectToFrame(video.frameId);
      } catch {
        waitForNextVideo();
        return;
      }
      portFrameId = video.frameId;
      if (leavingPort !== undefined) disconnectQuietly(leavingPort);
    }
    if (tryPostToPage({ kind: "begin_handoff", request_id: place.beginRequestId, document: video.document, element: video.element }) === "port_dead") {
      waitForNextVideo();
    }
  }


  // ---------------------------------------------------------------- страница → плеер

  function handleFrameMessage(port: FramePort, message: unknown): void {
    if (port !== framePort || phase === "finished") return;
    // Изолированный мир уже проверил форму; повторяем дёшево — фон не доверяет чужим процессам вслепую.
    const event = parsePageEvent(message);
    if (event === undefined) {
      // Единственное, что приходит сюда не из MAIN-мира: своё донесение изолированного мира.
      const flood = parsePageFloodNotice(message);
      if (flood !== undefined) endForPageFault({ kind: "page_flood", overBudget: flood.over_budget });
      return;
    }
    switch (event.kind) {
      case "handoff_began":
        handleHandoffBegan(event);
        return;
      case "handoff_ended":
        handleHandoffEnded(event.cause);
        return;
      case "shadow_alive":
        // Пульс тени (решение 180): только он сбрасывает сторожа тишины. Плееру не пересылается —
        // это внутреннее дело экстеншена. Вне `active` сторож не слушает, пульс ничего не меняет.
        silenceWatch.notePulse();
        return;
      case "file_source":
        startFileSource(event.url, event.referrer_origin === null ? null : { origin: event.referrer_origin, policy: event.referrer_policy });
        return;
      case "next_video_held":
        // Донесение сторожа идёт мимо порта, через пограничника: по порту его не бывает.
        return;
      default:
        forwardStreamEvent(event);
    }
  }

  function handleHandoffBegan(event: Extract<PageEvent, { kind: "handoff_began" }>): void {
    if ((phase !== "beginning" && phase !== "moving") || event.request_id !== place.beginRequestId) return;
    beginTimer.cancel();
    if (phase === "moving" && event.outcome !== "started") {
      // Видео исчезло или стало защищённым, пока фон решал: ждём дальше, предел ожидания тот же.
      waitForNextVideo();
      return;
    }
    if (event.outcome !== "started") {
      // Элемент исчез, потерял MediaSource или фрейм уже показывает другой документ (метка не та):
      // видео для передачи нет, а для выданного сеанса страница ушла.
      // Защищённый элемент (DRM появился между опросом и командой, 09c) — отдельный исход: видео
      // остаётся играть в браузере. Сеанс выдан, поэтому закрывается через `tab_gone`, как и остальные.
      abandonGrantedSession(outcomeOfRefusedBegin(event.outcome), "navigated_away");
      return;
    }
    phase = "active";
    silenceWatch.start();
    relocation.videoStarted(incomingVideo);
    adMirror.videoStarted();
    resolveOutcome({ kind: "started" });
  }

  /** Исход команды по отказу страницы начать тень. */
  function outcomeOfRefusedBegin(refused: Exclude<HandoffStartOutcome, "started">): HandOffTabOutcome {
    return refused === "protected" ? { kind: "protected" } : { kind: "no_video" };
  }

  function handleHandoffEnded(cause: HandoffEndCause): void {
    if (phase === "ending") {
      completeEnding();
      return;
    }
    // В `moving` это прежняя тень: мы сами её сняли. В `between_videos` тени уже нет.
    if (phase !== "active") return;
    if (cause === "element_released") {
      if (relocation.nextVideoExpectedAfterLoss()) {
        waitForNextVideo();
        return;
      }
      // Страница честно забрала видео (не-MSE источник, пустой источник дольше допуска, элемент
      // убран), вкладка на месте — решение 179: «The page took the video back», без доигрывания.
      endOnPageSide("page_took_video_back", { kind: "page_took_video_back" });
      return;
    }
    if (cause === "page_flood") {
      // Счётчик копий в MAIN-мире или воркере отказал (решение 200): та же вина страницы, что и бюджет событий.
      endOnPageSide("protocol_violation", { kind: "page_flood", overBudget: "forwarded_bytes" });
      return;
    }
    // `returned`, о котором мы не просили: тень снял сбой внутри страницы (отправка куска, старт
    // тени — `#finishActive` контроллера). Видео уже у страницы → для плеера это ошибка потока.
    endOnPageSide("protocol_violation", { kind: "page_side_failure" });
  }

  function forwardStreamEvent(event: StreamPageEvent): void {
    if (!acceptsStream() || translator === undefined || socket === undefined) return;
    const translated = translator.translate(event);
    if (!translated.ok) {
      rejectPageStream(translated.problem);
      return;
    }
    // Новое поколение MSE после поколения-файла (находка 12 ревью 09b): загрузчик прошлого файла
    // больше не нужен — его текущий запрос обрывается, а не докачивает ненужный файл.
    if (translated.items.some(startsGeneration)) disposeFileFeed();
    for (const item of translated.items) socket.enqueue(item);
  }

  /** Поток страницы идёт в плеер: тень стоит, встаёт или сеанс как раз переезжает. */
  function acceptsStream(): boolean {
    return phase === "beginning" || phase === "active" || phase === "between_videos" || phase === "moving";
  }

  /** Загрузчик диапазонов поколения-файла больше не нужен (новое поколение или конец сеанса). */
  function disposeFileFeed(): void {
    fileFeed?.dispose();
    fileFeed = undefined;
  }

  /** Поток страницы битый: честно закрываем сеанс (плеер закрыл бы 4003) и снимаем тень. */
  function rejectPageStream(problem: PageStreamProblem): void {
    socket?.close(SESSION_CLOSE_CODES.protocol_violation);
    if (phase === "active") {
      endHandoff(END_WITHOUT_PLAYER, { kind: "page_stream_problem", problem });
    } else if (phase === "between_videos" || phase === "moving") {
      tryPostToPage(END_WITHOUT_PLAYER);
      finish({ kind: "page_stream_problem", problem });
    } else {
      failStart({ kind: "failed", reason: `page_stream_problem:${problem}` });
    }
  }

  /**
   * Страница передаёт файл `<video src>`: новое поколение-файл, байты по запросам плеера качает
   * загрузчик диапазонов. Адрес остаётся в фоне — плееру уходит только номер поколения.
   */
  function startFileSource(url: string, referrer: FileReferrer | null): void {
    if (!acceptsStream() || translator === undefined || socket === undefined) return;
    // Проверка при выборе видео шла по словам кандидата, а адрес называет страница сейчас: сверяем.
    if (!ruleAllowsFileUrl(place.fileAddressRule, url)) {
      refuseFileAddress();
      return;
    }
    const translated = translator.translateFileSource();
    if (!translated.ok) {
      rejectPageStream(translated.problem);
      return;
    }
    for (const item of translated.items) socket.enqueue(item);
    // Новый файл в том же сеансе: прежний загрузчик больше не нужен (его запросы — прошлого поколения).
    disposeFileFeed();
    // Сначала разрешение заслона локальных адресов (сам запрос), снаружи него — Referer.
    const fileAddressRule = place.fileAddressRule;
    const network = dependencies.localAddressBlock.around(dependencies.fetchRange, fileAddressRule);
    fileFeed = createRangeFetcher({
      url,
      fetch: referrer === null ? network : dependencies.fileReferrer.around(network, referrer),
      allowsAddress: (finalUrl) => ruleAllowsFileUrl(fileAddressRule, finalUrl),
      sink: socket,
      scheduler: dependencies.scheduler,
    });
  }

  /** Страница назвала запрещённый адрес файла: сеанс кончается, ни одного запроса туда не ушло. */
  function refuseFileAddress(): void {
    if (phase === "beginning") {
      failStart({ kind: "local_address_refused" }, SESSION_CLOSE_CODES.protocol_violation);
    } else {
      endForPageFault({ kind: "local_address_refused" });
    }
  }

  // ---------------------------------------------------------------- плеер → страница

  function handlePlayerMessage(message: PlayerSessionMessage): void {
    // До старта тени и после её снятия командовать некем.
    if (phase !== "active" && phase !== "between_videos" && phase !== "moving") return;
    const action = translatePlayerMessage(message, numbering.latestGeneration());
    switch (action.kind) {
      case "shadow":
        if (action.command.kind === "clock") relocation.noteClockState(action.command.state);
        // Между роликами часы некому показывать: тени нет.
        if (phase === "active") commandPage({ kind: "shadow", command: action.command });
        return;
      case "end_handoff": {
        const end: HandoffSessionEnd = action.cause === "returned_to_page" ? { kind: "returned_to_page" } : { kind: "stream_rejected" };
        if (phase === "active") {
          endHandoff({ kind: "end_handoff", position: action.position, resume: action.resume }, end);
        } else {
          // Тени нет, снимать нечего и ждать подтверждения не от кого: зритель закрыл окно между роликами.
          pendingEnd = end;
          completeEnding();
        }
        return;
      }
      case "skip_ad":
        adMirror.skipAd();
        return;
      case "fetch_range":
        // Байты файла просят только в поколении-файле. Без файла запрос не наш (плеер ошибся
        // сам): его чтение честно истечёт у плеера, сеанс страницы это не ломает.
        fileFeed?.request(action.request);
        return;
      case "attached":
      case "buffer_intake":
      case "ignore":
        return;
    }
  }

  function handleSocketEnded(cause: SocketEndCause): void {
    if (cause.kind === "closed_by_extension") return; // мы сами закрыли — решение уже принято
    switch (phase) {
      case "opening":
        failStart({ kind: "player_unavailable", reason: `session_socket:${describeSocketEnd(cause)}` });
        return;
      case "abandoning":
        // Сокет умер раньше attach: сеанс у плеера закрыт и так, исход уже сообщён.
        finish({ kind: "start_failed", outcome: abandoned?.outcome ?? { kind: "failed", reason: "abandoned" } });
        return;
      case "beginning":
        // Тень могла успеть встать: изолированный мир снимет её сам, когда порт закроется.
        failStart({ kind: "player_unavailable", reason: `session_socket:${describeSocketEnd(cause)}` });
        return;
      case "active":
        endHandoff(END_WITHOUT_PLAYER, { kind: "player_ended", cause });
        return;
      case "between_videos":
      case "moving":
        tryPostToPage(END_WITHOUT_PLAYER);
        finish({ kind: "player_ended", cause });
        return;
      case "ending":
      case "finished":
        return;
    }
  }

  // ---------------------------------------------------------------- уход страницы

  function handleFrameGone(port: FramePort): void {
    if (port !== framePort) return;
    endForLostFramePort();
  }

  /**
   * Порт к странице мёртв: пришёл его `onDisconnect` или команда в него не ушла (Д-7 — тогда
   * `onDisconnect` может и не прийти, а плеер ждал бы подтверждения перемотки до 60 с).
   */
  function endForLostFramePort(): void {
    if (phase === "finished") return;
    if (phase === "beginning") {
      abandonGrantedSession({ kind: "failed", reason: "page_navigated_away" }, tabRemoved ? "closed" : "navigated_away");
      return;
    }
    if (tabRemoved) {
      leavePage("closed");
      return;
    }
    if (framePortLossIsSurvivable()) {
      askWhetherPageStayed();
      return;
    }
    // Порт закрывается раньше, чем приходит tabs.onRemoved: живость вкладки спрашиваем у браузера.
    if (askingWhetherTabExists) return;
    askingWhetherTabExists = true;
    dependencies.tabExists(target.tabId).then(
      (exists) => leavePage(exists && !tabRemoved ? "navigated_away" : "closed"),
      () => leavePage("closed"),
    );
  }

  /**
   * Порт рекламного фрейма умер. Убрали ролик — ждём следующий. Человек ушёл на другой адрес — видно
   * по адресу вкладки: фрейм умер вместе со страницей, и новый сайт сеанс подхватывать не должен.
   */
  function askWhetherPageStayed(): void {
    framePort = undefined;
    silenceWatch.stop();
    if (askingWhetherTabExists) return;
    askingWhetherTabExists = true;
    const answer = (tabUrl: string | undefined | null): void => {
      askingWhetherTabExists = false;
      if (phase !== "active" && phase !== "between_videos" && phase !== "moving") return;
      if (tabUrl === null) leavePage("closed");
      else if (tabUrl !== target.tabUrl) leavePage("navigated_away");
      else if (phase !== "between_videos") waitForNextVideo();
    };
    dependencies.describeTab(target.tabId).then(
      (tab) => answer(tab === undefined ? null : tab.url),
      () => answer(null),
    );
  }

  /**
   * Рекламный фрейм убрали вместе с роликом: это конец ролика, а не страницы. Главный документ вкладки
   * так не теряется: его порт умер, значит, страница ушла.
   */
  function framePortLossIsSurvivable(): boolean {
    if (portFrameId === TOP_FRAME_ID) return false;
    return phase === "between_videos" || phase === "moving" || (phase === "active" && relocation.nextVideoExpectedAfterLoss());
  }

  function leavePage(reason: TabGoneReason): void {
    if (phase === "finished") return;
    socket?.sendTabGoneAndClose(reason);
    finish({ kind: "page_gone", reason });
  }

  function notifyTabRemoved(): void {
    tabRemoved = true;
    if (phase === "beginning" || (phase === "opening" && socket !== undefined)) {
      abandonGrantedSession({ kind: "failed", reason: "tab_closed" }, "closed");
    } else if (phase === "abandoning" && abandoned !== undefined) {
      abandoned = { outcome: abandoned.outcome, reason: "closed" };
    } else if (phase === "active" || phase === "ending" || phase === "between_videos" || phase === "moving") {
      leavePage("closed");
    }
    // opening без сокета: openPlayerSession увидит флаг, когда плеер выдаст сеанс.
  }

  // ---------------------------------------------------------------- помощники

  /** Отправка странице без последствий; `port_dead` — порта нет или `postMessage` бросил. */
  function tryPostToPage(command: PageCommand): PageDelivery {
    if (framePort === undefined) return "port_dead";
    try {
      framePort.postMessage(command);
      return "delivered";
    } catch {
      // Отсоединённый порт бросает («disconnected port»): причина одна — порта больше нет.
      return "port_dead";
    }
  }

  /** Команда, потеря которой рвёт согласие с тенью: не ушла — сеанс кончается сразу (Д-7). */
  function commandPage(command: PageCommand): void {
    if (tryPostToPage(command) === "port_dead") endForLostFramePort();
  }

  openPlayerSession().catch((error: unknown) => failStart({ kind: "failed", reason: `open_session_threw:${String(error)}` }));
  return { outcome, notifyTabRemoved, heldVideo, adOnScreen: () => relocation.adOnScreen(), adWatchReport: adMirror.acceptFrameReport };
}
