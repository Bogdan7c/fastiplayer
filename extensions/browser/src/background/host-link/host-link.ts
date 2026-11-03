// Связь фона с посредником (native messaging): единственный владелец порта.
//
// Состояния (phase):
//   idle        — порта нет, ничего не ждём;
//   probing     — проба наличия посредника: порт открыт, сообщений нет (плеер не будится);
//   handshaking — отправлен `hello`, ждём ответ (таймаут HELLO_REPLY_TIMEOUT_MS);
//   ready       — `hello` принят, порт жив, можно слать `open_session`;
//   backoff     — связь упала, ждём паузу перед повтором (только пока есть нужда).
// Инварианты: порт один; у каждого таймера свой слот (повтор, ответ, проба, простой);
// повторы идут только пока есть нужда (acquireDemand), а нужда снята => таймер повтора отменён.
import { parseCapabilityTokens, type VideoCodecFamily } from "../../protocol/capability.ts";
import type { BridgeStatus } from "../../shared/bridge-status.ts";
import type { BridgeStatusProvider } from "../status-provider.ts";
import {
  CURRENT_PROTOCOL_VERSION,
  decodePlayerControl,
  encodeExtensionControl,
  type PlayerControlMessage,
  type PlayerRefused,
} from "../../protocol/index.ts";
import { HELLO_REPLY_TIMEOUT_MS, HOST_STARTUP_WINDOW_MS, PORT_IDLE_CLOSE_MS, RETRY_DELAYS_MS } from "./constants.ts";
import type { NativePort, NativePortFactory, Scheduler } from "./native-port.ts";
import {
  createOpenSessionRequests,
  type OpenSessionOutcome,
  type OpenSessionRequest,
  type PendingRequest,
} from "./open-session-requests.ts";
import { createStatusHub } from "./status-hub.ts";
import { createTimerSlot } from "./timer-slot.ts";

export interface HostLinkDeps {
  readonly portFactory: NativePortFactory;
  readonly scheduler: Scheduler;
  readonly hostName: string;
  // Версия экстеншена из манифеста (идёт в hello.software_version, только для диагностики).
  readonly extensionVersion: string;
  // Плеер сообщил «дешёвые» видео-кодеки (метки `cheap_codec.*` из hello_accepted; решение 43).
  // Зовётся на КАЖДЫЙ принятый hello, в том числе с пустым набором («метка снята»).
  readonly onPlayerCheapVideoCodecs?: (families: ReadonlySet<VideoCodecFamily>) => void;
  // Диагностика без содержимого сообщений. По умолчанию — console.warn.
  readonly warn?: (message: string) => void;
}

export interface HostLink extends BridgeStatusProvider {
  // Проба «посредник установлен?» при старте браузера/установке. Плеер не будится.
  probeHostPresence(): void;
  // Запрос сеанса (для сессии 10). Промис всегда завершается: ответом, отказом или таймаутом.
  openSession(request: OpenSessionRequest): Promise<OpenSessionOutcome>;
}

type Phase = "idle" | "probing" | "handshaking" | "ready" | "backoff";

// Живое соединение. Событие со старым id (порт уже заменён/закрыт) игнорируется.
interface Connection {
  readonly id: number;
  port: NativePort | undefined; // undefined только внутри connect()
  readonly openedAtMs: number;
  receivedAnyMessage: boolean;
}

export function createHostLink(deps: HostLinkDeps): HostLink {
  const { portFactory, scheduler } = deps;
  const warn = deps.warn ?? ((message: string) => console.warn(`[host-link] ${message}`));

  const statusHub = createStatusHub({ state: "unknown" });
  const retryTimer = createTimerSlot(scheduler);
  const replyTimer = createTimerSlot(scheduler);
  const probeTimer = createTimerSlot(scheduler);
  const idleTimer = createTimerSlot(scheduler);
  const requests = createOpenSessionRequests(scheduler, () => scheduleIdleCloseIfIdle());

  let phase: Phase = "idle";
  let connection: Connection | undefined;
  let nextConnectionId = 1;
  let nextRequestId = 1;
  let demandCount = 0;
  let retryIndex = 0;

  const publishConnectionLoss = (status: BridgeStatus): void => {
    statusHub.publish(status);
    requests.settleAllOnLinkLoss(status);
  };

  // ---------------------------------------------------------------- порт

  // Открывает порт. false — соединения нет: отказ уже обработан (failLink / handleDisconnect).
  function openConnection(): boolean {
    // Запись создаётся ДО connect(): браузер может закрыть порт синхронно, и разрыв должен найти запись.
    const record: Connection = { id: nextConnectionId++, port: undefined, openedAtMs: scheduler.now(), receivedAnyMessage: false };
    connection = record;
    try {
      const port = portFactory.connect(deps.hostName, {
        onMessage: (message) => handleMessage(record.id, message),
        onDisconnect: (errorText) => handleDisconnect(record.id, errorText),
      });
      if (connection !== record) {
        // Порт закрылся прямо внутри connect(): разрыв уже обработан, освобождать нечего.
        return false;
      }
      record.port = port;
      return true;
    } catch (error) {
      warn(`connectNative отказал: ${String(error)}`);
      if (connection === record) connection = undefined;
      failLink({ state: "host_missing" });
      return false;
    }
  }

  function closeConnection(): void {
    const closing = connection;
    connection = undefined;
    if (closing === undefined) return;
    try {
      closing.port?.disconnect();
    } catch (error) {
      warn(`порт не закрылся чисто: ${String(error)}`);
    }
  }

  // Отправка сообщения в порт; false — порт уже мёртв (событие разрыва обработано внутри).
  function sendToPort(text: string): boolean {
    const active = connection;
    if (active === undefined) return false;
    try {
      if (active.port === undefined) return false;
      active.port.postMessage(JSON.parse(text));
      return true;
    } catch (error) {
      warn(`postMessage не удался: ${String(error)}`);
      handleDisconnect(active.id, "postMessage failed");
      return false;
    }
  }

  // ---------------------------------------------------------------- нужда и простой

  const hasDemand = (): boolean => demandCount > 0;
  const isBusy = (): boolean => hasDemand() || requests.size() > 0;

  function scheduleIdleCloseIfIdle(): void {
    const canIdle = connection !== undefined && (phase === "ready" || phase === "handshaking");
    if (isBusy() || !canIdle) return;
    idleTimer.start(PORT_IDLE_CLOSE_MS, closeBecauseIdle);
  }

  function closeBecauseIdle(): void {
    if (isBusy()) return;
    replyTimer.cancel();
    closeConnection();
    phase = "idle";
    // «Подключён/проверяем» без живого порта уже не правда; ошибочные статусы остаются (значок «!»).
    const state = statusHub.current().state;
    if (state === "connected" || state === "checking") statusHub.publish({ state: "unknown" });
  }

  // ---------------------------------------------------------------- hello

  function startHandshake(): void {
    retryTimer.cancel();
    idleTimer.cancel();
    probeTimer.cancel();
    // Живой порт (проба или прежняя связь) переиспользуем: hello просто уходит в него.
    if (connection === undefined && !openConnection()) return;
    phase = "handshaking";
    if (statusHub.current().state === "unknown") statusHub.publish({ state: "checking" });
    const hello = encodeExtensionControl({
      type: "hello",
      protocol: CURRENT_PROTOCOL_VERSION,
      software_version: deps.extensionVersion,
      capabilities: [],
    });
    if (!hello.ok) {
      // Ошибка в нашей собственной константе — баг сборки; не молчим и не зацикливаемся.
      warn(`hello не прошёл кодек: ${hello.error.kind}`);
      failLink({ state: "player_unavailable", reason: "no_reply" });
      return;
    }
    replyTimer.start(HELLO_REPLY_TIMEOUT_MS, () => failLink({ state: "player_unavailable", reason: "no_reply" }));
    sendToPort(hello.value);
  }

  function scheduleRetry(): void {
    const delayMs = RETRY_DELAYS_MS[Math.min(retryIndex, RETRY_DELAYS_MS.length - 1)] ?? 30_000;
    retryIndex += 1;
    phase = "backoff";
    retryTimer.start(delayMs, () => {
      if (!hasDemand()) {
        phase = "idle";
        return;
      }
      startHandshake();
    });
  }

  // Связь не удалась: закрыть порт, объявить статус, разрешить ожидающих и (если нужда есть) запланировать повтор.
  function failLink(statusAfterFailure: BridgeStatus, options: { readonly retry: boolean } = { retry: true }): void {
    replyTimer.cancel();
    probeTimer.cancel();
    idleTimer.cancel();
    closeConnection();
    phase = "idle";
    publishConnectionLoss(statusAfterFailure);
    if (options.retry && hasDemand()) scheduleRetry();
  }

  // ---------------------------------------------------------------- входящее

  function decodeIncoming(rawMessage: unknown): PlayerControlMessage | undefined {
    // Браузер уже разобрал JSON; для строгости возвращаем в текст и разбираем кодеком протокола.
    let text: string | undefined;
    try {
      text = JSON.stringify(rawMessage);
    } catch {
      text = undefined;
    }
    if (text === undefined) {
      warn("ответ посредника не сериализуется");
      return undefined;
    }
    const decoded = decodePlayerControl(text);
    if (!decoded.ok) {
      warn(`ответ посредника отклонён: ${decoded.error.kind}`);
      return undefined;
    }
    return decoded.value;
  }

  function handleMessage(connectionId: number, rawMessage: unknown): void {
    if (connection === undefined || connection.id !== connectionId) return;
    connection.receivedAnyMessage = true;
    const message = decodeIncoming(rawMessage);
    if (message === undefined) {
      // Мусор. В ожидании ответа на hello — это «нет внятного ответа»; в ready — просто игнор.
      if (phase === "handshaking") failLink({ state: "player_unavailable", reason: "no_reply" });
      return;
    }
    switch (message.type) {
      case "hello_accepted":
        handleHelloAccepted(message);
        return;
      case "session_granted": {
        const pending = requests.get(message.request_id);
        if (pending === undefined || !pending.sent || pending.sessionKey !== message.session_key) {
          warn("session_granted без соответствующего запроса");
          if (phase === "handshaking") failLink({ state: "player_unavailable", reason: "no_reply" });
          return;
        }
        requests.settle(message.request_id, { kind: "granted", grant: message });
        return;
      }
      case "refused":
        handleRefused(message);
        return;
    }
  }

  function handleHelloAccepted(message: Extract<PlayerControlMessage, { type: "hello_accepted" }>): void {
    if (phase !== "handshaking") {
      warn("hello_accepted вне ожидания hello");
      return;
    }
    replyTimer.cancel();
    retryIndex = 0; // успех сбрасывает паузы
    phase = "ready";
    statusHub.publish({
      state: "connected",
      playerVersion: message.player.software_version,
      agreedProtocol: { major: message.agreed_protocol.major, minor: message.agreed_protocol.minor },
    });
    reportCheapCodecs(message.player.capabilities);
    for (const pending of requests.unsent()) sendRequest(pending);
    scheduleIdleCloseIfIdle();
  }

  // Метка плеера уходит наружу; сбой получателя не должен ломать сам hello.
  function reportCheapCodecs(capabilityTokens: readonly string[]): void {
    try {
      deps.onPlayerCheapVideoCodecs?.(parseCapabilityTokens(capabilityTokens).capabilities.cheapVideoCodecs);
    } catch (error) {
      warn(`получатель метки кодеков бросил: ${String(error)}`);
    }
  }

  function handleRefused(message: PlayerRefused): void {
    const target = message.in_reply_to;
    if (target.kind === "open_session") {
      handleOpenSessionRefused(target.request_id, message);
      return;
    }
    if (target.kind === "hello" && phase === "handshaking") {
      handleHelloRefused(message);
      return;
    }
    warn(`refused (${target.kind}) вне ожидания`);
    if (phase === "handshaking") failLink({ state: "player_unavailable", reason: "no_reply" });
  }

  function handleHelloRefused(message: PlayerRefused): void {
    const reason = message.reason;
    switch (reason.kind) {
      case "incompatible_version":
        // Обновление нужно человеку — повторы бессмысленны.
        failLink({ state: "incompatible", update: reason.update }, { retry: false });
        return;
      case "player_unavailable":
      case "player_not_responding":
      case "player_shutting_down":
        failLink({ state: "player_unavailable", reason: reason.kind });
        return;
      default:
        warn(`неожиданная причина отказа на hello: ${reason.kind}`);
        failLink({ state: "player_unavailable", reason: "no_reply" });
    }
  }

  function handleOpenSessionRefused(requestId: number, message: PlayerRefused): void {
    const pending = requests.get(requestId);
    if (pending === undefined) {
      warn("отказ на неизвестный open_session");
      return;
    }
    // Плеер перезапустился между звонками: сами повторяем hello и затем тот же запрос, один раз.
    // Если в полёте было несколько запросов, hello_required придёт на каждый: повторный hello
    // шлём только на первый (phase === "ready"), остальные просто ждут его ответа и уйдут
    // заново по hello_accepted (порядок ответов в порту совпадает с порядком отправки).
    const canRetryAfterHello = phase === "ready" || phase === "handshaking";
    if (message.reason.kind === "hello_required" && !pending.helloRetried && canRetryAfterHello) {
      pending.helloRetried = true;
      requests.markUnsent(requestId);
      if (phase === "ready") startHandshake();
      return;
    }
    requests.settle(requestId, { kind: "refused", refusal: message });
  }

  // ---------------------------------------------------------------- разрыв порта

  function handleDisconnect(connectionId: number, errorText: string | undefined): void {
    const closed = connection;
    // Событие от уже замененного/закрытого порта — игнор.
    if (closed === undefined || closed.id !== connectionId) return;
    connection = undefined;
    idleTimer.cancel();
    if (errorText !== undefined) warn(`порт закрыт: ${errorText}`);
    const wasPhase = phase;
    replyTimer.cancel();
    probeTimer.cancel();

    if (wasPhase === "probing") {
      phase = "idle";
      statusHub.publish({ state: "host_missing" });
      return;
    }
    // Правило host_missing: порт закрылся, не сказав НИ слова, в первую секунду — посредник не запустился
    // (настоящий всегда отвечает хотя бы refused). Иначе — посредник работал, но связи нет: no_reply.
    const closedBeforeAnyReply = !closed.receivedAnyMessage;
    const withinStartup = scheduler.now() - closed.openedAtMs < HOST_STARTUP_WINDOW_MS;
    const neverStarted = closedBeforeAnyReply && withinStartup;
    if (wasPhase === "ready" && !isBusy()) {
      // Порт упал, пока связь никому не нужна: просто забываем «подключён».
      phase = "idle";
      statusHub.publish({ state: "unknown" });
      return;
    }
    failLink(neverStarted ? { state: "host_missing" } : { state: "player_unavailable", reason: "no_reply" });
  }

  // ---------------------------------------------------------------- open_session

  function sendRequest(pending: PendingRequest): void {
    requests.markSent(pending.requestId);
    // Если порт мёртв, sendToPort сам обработает разрыв и разрешит запрос.
    sendToPort(pending.messageText);
  }

  function openSession(request: OpenSessionRequest): Promise<OpenSessionOutcome> {
    const requestId = nextRequestId++;
    const encoded = encodeExtensionControl({ type: "open_session", request_id: requestId, ...request });
    if (!encoded.ok) return Promise.resolve({ kind: "invalid_request", error: encoded.error });

    return new Promise<OpenSessionOutcome>((resolve) => {
      const pending: PendingRequest = {
        requestId,
        sessionKey: request.session_key,
        messageText: encoded.value,
        sent: false,
        helloRetried: false,
      };
      requests.add(pending, resolve);
      idleTimer.cancel();
      if (phase === "ready") {
        sendRequest(pending);
      } else if (phase !== "handshaking") {
        // idle / probing / backoff: действие человека не ждёт паузы — пробуем сразу.
        startHandshake();
      }
      // handshaking: запрос отправится по hello_accepted или получит not_connected при неудаче.
    });
  }

  // ---------------------------------------------------------------- проба и нужда

  function probeHostPresence(): void {
    if (phase !== "idle" || isBusy()) return;
    phase = "probing";
    if (!openConnection()) return;
    // Порт прожил окно — посредник существует. Закрываем, не послав ни одного сообщения.
    probeTimer.start(HOST_STARTUP_WINDOW_MS, () => {
      closeConnection();
      phase = "idle";
      if (statusHub.current().state === "host_missing") statusHub.publish({ state: "unknown" });
    });
  }

  function acquireDemand(_reason: string): () => void {
    demandCount += 1;
    if (demandCount === 1) {
      idleTimer.cancel();
      // Backoff при нужде уже идёт сам; handshaking ждём. В ready с запросом в полёте плеер и так
      // отвечает прямо сейчас: лишний hello мог бы своим таймаутом оборвать исправный запрос.
      // Остальное (idle/probing/ready без запросов) — проверяем по-настоящему.
      const answerIsAlreadyInFlight = phase === "ready" && requests.hasSent();
      if (phase !== "backoff" && phase !== "handshaking" && !answerIsAlreadyInFlight) startHandshake();
    }
    let released = false;
    return () => {
      if (released) return;
      released = true;
      demandCount -= 1;
      if (demandCount > 0) return;
      retryTimer.cancel();
      if (phase === "backoff") phase = "idle";
      scheduleIdleCloseIfIdle();
    };
  }

  return {
    current: statusHub.current,
    subscribe: statusHub.subscribe,
    acquireDemand,
    probeHostPresence,
    openSession,
  };
}
