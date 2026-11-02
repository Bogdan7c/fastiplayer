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
//   тоже (позиция тени, «как тень»); уход со страницы (порт закрылся) и закрытие вкладки шлют
//   плееру `tab_gone`; `handoff_ended{element_released}` от страницы = «ушли со страницы».
// Конец сообщается владельцу ровно один раз (`onFinished`), нужда связи освобождается ровно один раз.

import type { HandOffTabOutcome, PageCommand, PageEvent } from "../../shared/handoff-contract.ts";
import { parsePageEvent } from "../../shared/page-message-validation.ts";
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
import type { FrameConnector, FramePort } from "./frame-port.ts";
import { createDocumentTranslator, type DocumentTranslator, type PageStreamProblem } from "./page-event-translator.ts";
import { translatePlayerMessage } from "./player-message-translator.ts";
import { NORMAL_CLOSE_CODE, openSessionSocket, type SessionSocket, type SocketEndCause, type WebSocketFactory } from "./session-socket.ts";
import { createStreamNumbering, type StreamNumbering } from "./stream-numbering.ts";

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
}

/** Чем закончилась передача (для владельца и тестов; журнала в продукте нет). */
export type HandoffSessionEnd =
  | { readonly kind: "start_failed"; readonly outcome: HandOffTabOutcome }
  | { readonly kind: "page_gone"; readonly reason: TabGoneReason }
  | { readonly kind: "element_released" }
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
 * - active — тень стоит, поток идёт;
 * - ending — мы послали `end_handoff`, ждём `handoff_ended{returned}` (или таймаут);
 * - finished — всё освобождено.
 */
type SessionPhase = "opening" | "abandoning" | "beginning" | "active" | "ending" | "finished";

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
  const releaseDemand = dependencies.door.acquireDemand(DEMAND_REASON);
  let phase: SessionPhase = "opening";
  let tabRemoved = false;
  let socket: SessionSocket | undefined;
  let framePort: FramePort | undefined;
  let translator: DocumentTranslator | undefined;
  // Чем кончится передача, если страница подтвердит снятие тени (запоминается при `end_handoff`).
  let pendingEnd: HandoffSessionEnd | undefined;
  // Плеер ответил `attached` на наш `attach`.
  let socketAttached = false;
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
    // Предел ожидания ставится ДО отправки: если порт уже мёртв (отправка не удалась) и его
    // onDisconnect не придёт, сеанс всё равно закончится сам, а не повиснет в `ending`.
    endTimer.start(HANDOFF_END_REPLY_TIMEOUT_MS, () => completeEnding());
    postToPage(command);
  }

  function completeEnding(): void {
    socket?.close(NORMAL_CLOSE_CODE);
    finish(pendingEnd ?? { kind: "returned_to_page" });
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
    postToPage({ kind: "begin_handoff", request_id: target.beginRequestId, document: target.document, element: target.element });
  }

  // ---------------------------------------------------------------- страница → плеер

  function handleFrameMessage(port: FramePort, message: unknown): void {
    if (port !== framePort || phase === "finished") return;
    // Изолированный мир уже проверил форму; повторяем дёшево — фон не доверяет чужим процессам вслепую.
    const event = parsePageEvent(message);
    if (event === undefined) return;
    switch (event.kind) {
      case "handoff_began":
        handleHandoffBegan(event);
        return;
      case "handoff_ended":
        handleHandoffEnded(event.cause);
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
      abandonGrantedSession({ kind: "no_video" }, "navigated_away");
      return;
    }
    phase = "active";
    resolveOutcome({ kind: "started" });
  }

  function handleHandoffEnded(cause: "returned" | "element_released"): void {
    if (phase === "ending") {
      completeEnding();
      return;
    }
    if (phase !== "active") return;
    if (cause === "element_released") {
      // Страница сама убрала источник (новый src, load(), удаление) — для плеера это уход со страницы.
      socket?.sendTabGoneAndClose("navigated_away");
      finish({ kind: "element_released" });
      return;
    }
    socket?.close(NORMAL_CLOSE_CODE);
    finish({ kind: "returned_to_page" });
  }

  function forwardStreamEvent(event: Exclude<PageEvent, { kind: "handoff_began" } | { kind: "handoff_ended" }>): void {
    if ((phase !== "beginning" && phase !== "active") || translator === undefined || socket === undefined) return;
    const translated = translator.translate(event);
    if (!translated.ok) {
      // Поток страницы битый: честно закрываем сеанс (плеер закрыл бы 4003) и снимаем тень.
      socket.close(SESSION_CLOSE_CODES.protocol_violation);
      if (phase === "active") {
        endHandoff(END_WITHOUT_PLAYER, { kind: "page_stream_problem", problem: translated.problem });
      } else {
        failStart({ kind: "failed", reason: `page_stream_problem:${translated.problem}` });
      }
      return;
    }
    for (const item of translated.items) socket.enqueue(item);
  }

  // ---------------------------------------------------------------- плеер → страница

  function handlePlayerMessage(message: PlayerSessionMessage): void {
    if (phase !== "active") return; // до старта тени и после её снятия командовать некем
    const action = translatePlayerMessage(message, numbering.latestGeneration());
    switch (action.kind) {
      case "shadow":
        postToPage({ kind: "shadow", command: action.command });
        return;
      case "end_handoff":
        endHandoff(
          { kind: "end_handoff", position: action.position, resume: action.resume },
          action.cause === "returned_to_page" ? { kind: "returned_to_page" } : { kind: "stream_rejected" },
        );
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
    if (port !== framePort || phase === "finished") return;
    if (phase === "beginning") {
      abandonGrantedSession({ kind: "failed", reason: "page_navigated_away" }, tabRemoved ? "closed" : "navigated_away");
      return;
    }
    if (tabRemoved) {
      leavePage("closed");
      return;
    }
    // Порт закрывается раньше, чем приходит tabs.onRemoved: живость вкладки спрашиваем у браузера.
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

  /** Отправка странице; false — порт уже мёртв (его onDisconnect закончит сеанс). */
  function postToPage(command: PageCommand): boolean {
    if (framePort === undefined) return false;
    try {
      framePort.postMessage(command);
      return true;
    } catch {
      return false;
    }
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
