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

import type { HandOffTabOutcome, HandoffEndCause, HandoffStartOutcome, PageCommand, PageEvent, PageEventBudgetKind } from "../../shared/handoff-contract.ts";
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
import { createShadowSilenceWatch } from "./shadow-silence-watch.ts";
import { createStreamNumbering, type StreamNumbering } from "./stream-numbering.ts";

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
  /** Сеть браузера для байтов файла `<video src>` (сессия 09b; в продукте — `fetch` фона). */
  readonly fetchRange: RangeFetch;
  /** Referer закачки файла (решение 202): правило браузера на время одного запроса. */
  readonly fileReferrer: FileReferrerRules;
  /** Сетевой заслон локальных адресов (решение 199): разрешение сеанса на время одного запроса. */
  readonly localAddressBlock: LocalAddressBlock;
}

/** Что и куда передаём. */
export interface HandoffTarget {
  readonly tabId: number;
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
}

/** Чем закончилась передача (для владельца и тестов; журнала в продукте нет). */
export type HandoffSessionEnd =
  | { readonly kind: "start_failed"; readonly outcome: HandOffTabOutcome }
  | { readonly kind: "page_gone"; readonly reason: TabGoneReason }
  /** Страница сама забрала видео, вкладка на месте (`handoff_ended{element_released}`, 4009). */
  | { readonly kind: "page_took_video_back" }
  /** Страница сняла тень сама без нашей просьбы — сбой внутри страницы (4003, Д-6). */
  | { readonly kind: "page_side_failure" }
  /** Тень замолчала дольше предела пульса (4003, решение 180). */
  | { readonly kind: "shadow_silent" }
  /** Страница превысила бюджет событий или счётчик копий байтов (4003, решение 200); вид — только для журнала. */
  | { readonly kind: "page_flood"; readonly overBudget: PageEventBudgetKind | "forwarded_bytes" }
  /** Страница назвала адрес файла, который правило запрещает (4003, решение 199). */
  | { readonly kind: "local_address_refused" }
  | { readonly kind: "returned_to_page" }
  | { readonly kind: "stream_rejected" }
  | { readonly kind: "player_ended"; readonly cause: SocketEndCause }
  | { readonly kind: "page_stream_problem"; readonly problem: PageStreamProblem };

export interface HandoffSession {
  /** Исход старта: `started` или причина отказа. Всегда завершается. */
  readonly outcome: Promise<HandOffTabOutcome>;
  /** Браузер сообщил, что вкладка закрыта (`tabs.onRemoved`). */
  notifyTabRemoved(): void;
}

/**
 * Фазы:
 * - opening — ждём `open_session` и `attached`;
 * - abandoning — сеанс выдан, но страница пропала: ждём `attached`, чтобы честно послать `tab_gone`;
 * - beginning — порт к фрейму открыт, ждём `handoff_began`;
 * - active — тень стоит, поток идёт, сторож тишины слушает пульс тени;
 * - ending — мы послали `end_handoff`, ждём `handoff_ended{returned}` (или таймаут);
 * - finished — всё освобождено.
 */
type SessionPhase = "opening" | "abandoning" | "beginning" | "active" | "ending" | "finished";

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
  const releaseDemand = dependencies.door.acquireDemand(DEMAND_REASON);
  let phase: SessionPhase = "opening";
  let tabRemoved = false;
  let socket: SessionSocket | undefined;
  let framePort: FramePort | undefined;
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
    if (phase !== "active") return;
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

  function beginOnPage(): void {
    if (phase !== "opening") return;
    try {
      framePort = dependencies.connectFrame(target.tabId, target.frameId);
    } catch (error) {
      abandonGrantedSession({ kind: "failed", reason: `frame_connect_failed:${String(error)}` }, "navigated_away");
      return;
    }
    const port = framePort;
    // Новое подключение = новый документ: свои локальные ключи MediaSource, общий счёт поколений.
    translator = createDocumentTranslator(numbering);
    port.onMessage.addListener((message) => handleFrameMessage(port, message));
    port.onDisconnect.addListener(() => handleFrameGone(port));
    phase = "beginning";
    beginTimer.start(HANDOFF_BEGIN_REPLY_TIMEOUT_MS, () =>
      abandonGrantedSession({ kind: "failed", reason: "page_did_not_answer" }, "navigated_away"),
    );
    commandPage({ kind: "begin_handoff", request_id: target.beginRequestId, document: target.document, element: target.element });
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
      default:
        forwardStreamEvent(event);
    }
  }

  function handleHandoffBegan(event: Extract<PageEvent, { kind: "handoff_began" }>): void {
    if (phase !== "beginning" || event.request_id !== target.beginRequestId) return;
    beginTimer.cancel();
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
    if (phase !== "active") return;
    if (cause === "element_released") {
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
    if ((phase !== "beginning" && phase !== "active") || translator === undefined || socket === undefined) return;
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
    } else {
      failStart({ kind: "failed", reason: `page_stream_problem:${problem}` });
    }
  }

  /**
   * Страница передаёт файл `<video src>`: новое поколение-файл, байты по запросам плеера качает
   * загрузчик диапазонов. Адрес остаётся в фоне — плееру уходит только номер поколения.
   */
  function startFileSource(url: string, referrer: FileReferrer | null): void {
    if ((phase !== "beginning" && phase !== "active") || translator === undefined || socket === undefined) return;
    // Проверка при выборе видео шла по словам кандидата, а адрес называет страница сейчас: сверяем.
    if (!ruleAllowsFileUrl(target.fileAddressRule, url)) {
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
    const network = dependencies.localAddressBlock.around(dependencies.fetchRange, target.fileAddressRule);
    fileFeed = createRangeFetcher({
      url,
      fetch: referrer === null ? network : dependencies.fileReferrer.around(network, referrer),
      allowsAddress: (finalUrl) => ruleAllowsFileUrl(target.fileAddressRule, finalUrl),
      sink: socket,
      scheduler: dependencies.scheduler,
    });
  }

  /** Страница назвала запрещённый адрес файла: сеанс кончается, ни одного запроса туда не ушло. */
  function refuseFileAddress(): void {
    if (phase === "active") {
      endForPageFault({ kind: "local_address_refused" });
    } else {
      failStart({ kind: "local_address_refused" }, SESSION_CLOSE_CODES.protocol_violation);
    }
  }

  // ---------------------------------------------------------------- плеер → страница

  function handlePlayerMessage(message: PlayerSessionMessage): void {
    if (phase !== "active") return; // до старта тени и после её снятия командовать некем
    const action = translatePlayerMessage(message, numbering.latestGeneration());
    switch (action.kind) {
      case "shadow":
        commandPage({ kind: "shadow", command: action.command });
        return;
      case "end_handoff":
        endHandoff(
          { kind: "end_handoff", position: action.position, resume: action.resume },
          action.cause === "returned_to_page" ? { kind: "returned_to_page" } : { kind: "stream_rejected" },
        );
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
    // Порт закрывается раньше, чем приходит tabs.onRemoved: живость вкладки спрашиваем у браузера.
    if (askingWhetherTabExists) return;
    askingWhetherTabExists = true;
    dependencies.tabExists(target.tabId).then(
      (exists) => leavePage(exists && !tabRemoved ? "navigated_away" : "closed"),
      () => leavePage("closed"),
    );
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
    } else if (phase === "active" || phase === "ending") {
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
  return { outcome, notifyTabRemoved };
}

/** Снять тень без указаний плеера: позиция тени, играть/стоять как тень. */
const END_WITHOUT_PLAYER: Extract<PageCommand, { kind: "end_handoff" }> = {
  kind: "end_handoff",
  position: { kind: "shadow_position" },
  resume: { kind: "as_shadow" },
};

/** Отказ `open_session` → исход для попапа 10 (причина — машинная строка). */
function describeRefusal(outcome: Exclude<OpenSessionOutcome, { kind: "granted" }>): HandOffTabOutcome {
  switch (outcome.kind) {
    case "refused":
      return { kind: "player_unavailable", reason: outcome.refusal.reason.kind };
    case "no_reply":
      return { kind: "player_unavailable", reason: "no_reply" };
    case "not_connected": {
      const status = outcome.status;
      return { kind: "player_unavailable", reason: status.state === "player_unavailable" ? status.reason : status.state };
    }
    case "invalid_request":
      return { kind: "failed", reason: `invalid_open_session:${outcome.error.kind}` };
  }
}

function describeSocketEnd(cause: SocketEndCause): string {
  switch (cause.kind) {
    case "player_closed":
      return cause.reason ?? `code_${cause.code}`;
    case "player_protocol_violation":
    case "page_message_rejected":
      return `${cause.kind}:${cause.detail}`;
    default:
      return cause.kind;
  }
}

function disconnectQuietly(port: FramePort): void {
  try {
    port.disconnect();
  } catch {
    // Порт уже закрыт браузером — освобождать нечего.
  }
}
