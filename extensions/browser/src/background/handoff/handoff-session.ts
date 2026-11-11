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
// Сессия 11a: «Return» с заглушки (донесение изолированного мира `viewer_return_requested`, решение 236) →
// снять тень на её позиции, страница играет, сокет 4010 сразу; сломался плеер страницы
// (`handoff_ended{page_player_failed}`, решение 237) → как уход вкладки; окно закрыто зрителем без прощания
// → пауза (решение 238). Правила — в session-end-rules.ts.
// Конец сообщается владельцу ровно один раз (`onFinished`), нужда связи освобождается ровно один раз.
//
// Переезд на следующее видео (сессия 09c2, решения 201, 204): реклама кончилась или её пропустили, а
// сеанс живёт. Пока переезд открыт (`video-relocation.ts`), страница, убравшая видео, не значит «забрала
// видео»: фаза `between_videos`, окно держит последний кадр. Сторож задержал следующее видео — фаза
// `moving`: прежняя тень снимается, новая принимается той же командой `begin_handoff`. Другой фрейм —
// новый порт, номера поколений общие. Предел ожидания вышел — честный конец с кодом 4009.
//
// Возврат страницы (сессия 11b2, решения 240–241; владелец стадии — page-return/session-page-return.ts):
// уход страницы, которую можно вернуть, — фаза `page_away` и `page_left`, связь держим. По `reopen_page`
// страница открывается заново, тень встаёт тем же переездом (`moving`), но в другой вкладке.
//
// Смена серии на той же странице (сессия 16a; владелец правил — episode-change/): серия доиграла, а следующая
// не пришла за 60 с — 4011 (старому плееру 4009); пока ждём — `awaiting_next_episode`; заголовок вкладки —
// `page_title_changed`; одностраничник сменил адрес, а видео пропало — `tab_gone{navigated_away}` (решение 42).

import type {
  HandOffTabOutcome,
  HandoffEndCause,
  HeldVideoReply,
  HeldVideoReport,
  PageCommand,
  PageEvent,
  PlayerWindow,
  VideoRef,
} from "../../shared/handoff-contract.ts";
import { parsePageEvent, parsePageFloodNotice, parseViewerReturnRequest } from "../../shared/page-message-validation.ts";
import {
  PROTOCOL_VERSION_1_0,
  SESSION_CLOSE_CODES,
  type PlayerSessionMessage,
  type ProtocolVersion,
  type TabGoneReason,
} from "../../protocol/index.ts";
import { createTimerSlot } from "../host-link/timer-slot.ts";
import { HANDOFF_BEGIN_REPLY_TIMEOUT_MS, HANDOFF_END_REPLY_TIMEOUT_MS } from "./constants.ts";
import type { FileAddressRule } from "./file-address-guard.ts";
import type { FramePort } from "./frame-port.ts";
import { playerAbilitiesOf } from "./hls-player-support.ts";
import { createDocumentTranslator, type DocumentTranslator, type PageStreamProblem, type StreamPageEvent } from "./page-event-translator.ts";
import { choiceMemoryWritingOf, createSessionSiteAdapters } from "./session-site-adapters.ts";
import { DEMAND_REASON, NATIVE_HLS_NOT_YET, startsGeneration, UNKNOWN_HINTS, type PageDelivery, type SessionPlace } from "./handoff-session-basics.ts";
import type { HandoffSession, HandoffSessionEnd } from "./session-contract.ts";
import type { SessionPhase } from "./session-phase.ts";

export type { HandoffSession, HandoffSessionEnd } from "./session-contract.ts";
export type { HandoffSessionDependencies, HandoffTarget, SessionDoor, TabDescription } from "./handoff-session-dependencies.ts";
import type { HandoffSessionDependencies, HandoffTarget } from "./handoff-session-dependencies.ts";
import { translatePlayerMessage } from "./player-message-translator.ts";
import {
  NORMAL_CLOSE_CODE,
  openSessionSocket,
  type QueueDeliveringCloseReason,
  type SessionSocket,
  type SocketEndCause,
} from "./session-socket.ts";
import { referrerOf, startCheckedPulledSource, type PulledSourceEvent } from "./pulled-source-start.ts";
import { createSessionSourceFeeds, networkForSource } from "./session-source-feeds.ts";
import { createPageSubtitleRelay } from "./page-subtitles/page-subtitle-relay.ts";
import { judgeHeldVideo } from "./held-video-choice.ts";
import { createShadowSilenceWatch } from "./shadow-silence-watch.ts";
import { createStreamNumbering, type StreamNumbering } from "./stream-numbering.ts";
import type { ChosenVideo } from "./video-choice.ts";
import { createVideoRelocation, type NextVideoWait } from "./video-relocation.ts";
import { createSessionEpisodeChange } from "./episode-change/session-episode-change.ts";
import { endAfterSourceLoss, sourceLossOfExpiredWait, type SourceLossEnd } from "./episode-change/source-loss-end.ts";
import { describeRefusal, describeSocketEnd, outcomeOfRefusedBegin, playerWindowOf } from "./session-outcome-words.ts";
import { disconnectQuietly, END_WITHOUT_PLAYER, LEAVE_PREVIOUS_VIDEO, RETURN_PLAYING_AT_SHADOW } from "./session-page-commands.ts";
import { closingAfterConfirmedEnd, endCommandAfterPlayerSocketEnd, stepAfterPageEndedShadow } from "./session-end-rules.ts";
import { createSessionPageReturn } from "./page-return/session-page-return.ts";
import { createPageLeave } from "./session-page-leave.ts";
import { createPagePresence } from "./session-page-presence.ts";
import { createSessionPageReveal } from "./page-tab-reveal.ts";

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
    armFrames: (leaseMs) => dependencies.armNextVideo(presence.tabId(), leaseMs),
    onWaitExpired: (wait) => endForNoNextVideo(wait),
  });
  // Адаптеры сайтов (09d, 14b): реклама и меню вариантов — аренда наблюдения во фреймах, сводки, команды плеера.
  const siteAdapters = createSessionSiteAdapters({
    currentTabId: () => presence.tabId(),
    sessionFrameId: () => portFrameId,
    scheduler: dependencies.scheduler,
    browsers: dependencies,
    sessionSaysAdOnScreen: () => relocation.adOnScreen(),
    agreedProtocol: () => agreedProtocol,
    send: (message) => socket?.enqueue({ kind: "message", message }),
    note: dependencies.noteForDeveloper,
    choiceMemory: { store: dependencies.choiceMemory, pageUrl: () => presence.pageUrl(), writing: choiceMemoryWritingOf(target.incognito) },
  });
  const releaseDemand = dependencies.door.acquireDemand(DEMAND_REASON);
  // Вкладка страницы сеанса (после возврата страницы, 11b2, — другая) и вопросы браузеру о её уходе.
  const presence = createPagePresence({ tabId: target.tabId, tabUrl: target.tabUrl }, dependencies);
  const pageReveal = createSessionPageReveal({ api: dependencies.pageTabReveal, currentTabId: () => presence.tabId(), note: dependencies.noteForDeveloper });
  const pageReturn = createSessionPageReturn({
    browser: dependencies.pageReopen,
    scheduler: dependencies.scheduler,
    note: dependencies.noteForDeveloper,
    socket: () => socket,
    quietenPageSide: () => quietenForOrphan(),
    pageMovedToTab: (tabId, url) => {
      presence.moveTo(tabId, url);
      target.movePageToTab(tabId);
    },
    endSession: (end) => finish(end),
  });
  pageReturn.notePage(target.pagePlace);
  // Уход страницы (порт умер, вкладку закрыли, ушли со страницы): page-leave.ts; состоянием владеет сеанс.
  const pageLeave = createPageLeave({
    presence,
    pageReturn,
    relocation,
    silenceWatch,
    phase: () => phase,
    portFrameId: () => portFrameId,
    socketOpened: () => socket !== undefined,
    socketAttached: () => socketAttached,
    agreedProtocol: () => agreedProtocol,
    sendTabGoneAndClose: (reason) => socket?.sendTabGoneAndClose(reason),
    forgetFramePort: () => {
      framePort = undefined;
    },
    abandonGrantedSession: (reason, failure) => abandonGrantedSession({ kind: "failed", reason: failure }, reason),
    isAbandonmentPending: () => abandoned !== undefined,
    relabelAbandonmentAsClosed: () => {
      if (abandoned !== undefined) abandoned = { outcome: abandoned.outcome, reason: "closed" };
    },
    waitForNextVideo: () => waitForNextVideo(),
    finish: (end) => finish(end),
  });
  const { endForLostFramePort, leavePage, notifyTabRemoved } = pageLeave;
  // Смена серии на той же странице (16a): заголовок и адрес вкладки, «ждём следующую серию» — episode-change/.
  const episodeChange = createSessionEpisodeChange({
    watchTabUpdates: dependencies.watchTabUpdates,
    currentTabId: () => presence.tabId(),
    openedPage: { title: target.page.title, url: target.tabUrl },
    agreedProtocol: () => agreedProtocol,
    playerHearsPage: () => socketAttached && acceptsStream() && !pageReturn.isReturning(),
    send: (message) => socket?.enqueue({ kind: "message", message }),
  });
  let phase: SessionPhase = "opening";
  let place: SessionPlace = { frameId: target.frameId, beginRequestId: target.beginRequestId, fileAddressRule: target.fileAddressRule };
  // Видео, на которое идёт тень сейчас: его признаки войдут в силу с `handoff_began{started}`.
  let incomingVideo: Pick<ChosenVideo, "role" | "mayBeAd" | "durationS"> = target.video;
  // Что делать, если тень на принятое видео не встала: при переезде — ждать следующее, при возврате — отказ.
  let beginFallback: () => void = () => waitForNextVideo();
  // Элемент, на который идёт тень, и элемент в плеере (`null` — между видео).
  let incomingRef: VideoRef = { frame_id: target.frameId, document: target.document, element: target.element };
  let shownRef: VideoRef | null = null;
  // Окно плеера из ответа на `open_session` (решение 216).
  let playerWindow: PlayerWindow = "new";
  let socket: SessionSocket | undefined;
  let framePort: FramePort | undefined;
  // Фрейм живого порта.
  let portFrameId = target.frameId;
  let translator: DocumentTranslator | undefined;
  // Сеть браузера для всего, что фон качает сам: файл, HLS, `<track src>`.
  const sourceNetwork = { fetch: dependencies.fetchRange, fileReferrer: dependencies.fileReferrer, localAddressBlock: dependencies.localAddressBlock };
  // Загрузчики источников, которые фон качает сам: файл (09b) и нативный HLS (10a2); у MSE их нет.
  const feeds = createSessionSourceFeeds({ ...sourceNetwork, scheduler: dependencies.scheduler });
  // Субтитры страницы (13b2): номера дорожек живут на сеанс, а не на документ; свои закачки `<track src>`.
  const subtitles = createPageSubtitleRelay({
    sink: () => socket,
    agreedProtocol: () => agreedProtocol,
    streamIsOpen: acceptsStream,
    networkFor: (referrer) => networkForSource(sourceNetwork, { referrer, rule: place.fileAddressRule }),
    fileAddressRule: () => place.fileAddressRule,
    scheduler: dependencies.scheduler,
    note: dependencies.noteForDeveloper,
    onTrackOffered: (track) => siteAdapters.subtitleTrackOffered(track),
  });
  // Версия протокола, о которой договорились с плеером (из выдачи сеанса): новее неё сообщений не шлём.
  // До выдачи неизвестна; самая ранняя версия - безопасная сторона.
  let agreedProtocol: ProtocolVersion = PROTOCOL_VERSION_1_0;
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
    stopPageSide();
    episodeChange.stop();
    // Открытие страницы отменяется, открытая нами вкладка закрывается (11b2).
    pageReturn.cancel();
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

  /** Тень снята по нашему `end_handoff` (или страница не ответила вовремя) — закрыть сокет (session-end-rules.ts). */
  function completeEnding(): void {
    const end = pendingEnd ?? { kind: "returned_to_page" };
    const closing = closingAfterConfirmedEnd(end);
    if (closing.kind === "deliver_queue_then_close") socket?.closeAfterDeliveringQueue(closing.reason);
    else socket?.close(closing.code);
    finish(end);
  }

  /**
   * «Return» зрителя (решение 236). Только в `active`: в `ending` уже идёт другой конец (первый побеждает),
   * между роликами и при переезде заглушки нет. Окно закрывается СРАЗУ, не дожидаясь страницы: иначе оно
   * играло бы вместе со страницей, пока та возвращает видео.
   */
  function returnForViewer(): void {
    if (phase !== "active") {
      dependencies.noteForDeveloper(`viewer return ignored in phase ${phase}`);
      return;
    }
    endHandoff(RETURN_PLAYING_AT_SHADOW, { kind: "returned_by_viewer" });
    socket?.close(SESSION_CLOSE_CODES.viewer_returned_to_page);
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
    playerWindow = playerWindowOf(granted.grant.outcome);
    agreedProtocol = granted.agreedProtocol;
    socket = openSessionSocket({
      port: granted.grant.port,
      sessionKey: target.sessionKey,
      token: granted.grant.token,
      createWebSocket: dependencies.createWebSocket,
      scheduler: dependencies.scheduler,
      callbacks: { onAttached: handleAttached, onPlayerMessage: handlePlayerMessage, onEnded: handleSocketEnded },
    });
    // Вкладку закрыли, пока плеер выдавал сеанс: сеанс выдан — честно закрываем его через attach.
    if (presence.wasRemoved()) abandonGrantedSession({ kind: "failed", reason: "tab_closed" }, "closed");
  }

  function handleAttached(): void {
    socketAttached = true;
    if (phase === "abandoning" && abandoned !== undefined) {
      socket?.sendTabGoneAndClose(abandoned.reason);
      finish({ kind: "start_failed", outcome: abandoned.outcome });
      return;
    }
    const previousSessionEnded = target.afterPreviousSession;
    if (previousSessionEnded === undefined) return beginOnPage();
    // Замена: ждём конец старого сеанса; `beginOnPage` сам проверяет фазу, если сеанс успел кончиться.
    void previousSessionEnded().then(beginOnPage);
  }

  /** Открыть порт к фрейму и начать с ним новый документ. Бросает, если браузер не даёт порт. */
  function connectToFrame(frameId: number): void {
    const port = dependencies.connectFrame(presence.tabId(), frameId);
    framePort = port;
    // Новое подключение = новый документ: свои локальные ключи MediaSource, общий счёт поколений.
    translator = createDocumentTranslator(numbering);
    // Субтитры: номера по документу (фрейм + метка), не по порту; закачки прежнего подключения отменены.
    subtitles.beginDocument({ frameId, documentTag: incomingRef.document });
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
    shownRef = null;
    silenceWatch.stop();
    beginTimer.cancel();
    const wait = relocation.videoLeft();
    forwardStreamEvent({ kind: "page_playback", request: { kind: "pause" } });
    // Серия доиграла, страница ставит следующую новым элементом или файлом (Tortuga): плееру — «ждём» (16a).
    if (wait === "after_episode_end") episodeChange.nextEpisodeWaitBegan();
  }

  /**
   * Следующего видео нет дольше предела (решения 179, 307, 42): посреди ролика — «страница забрала видео»,
   * после конца серии — «не поставила следующую», адрес вкладки сменился — уход одностраничника.
   */
  function endForNoNextVideo(wait: NextVideoWait): void {
    if (phase !== "active" && phase !== "between_videos") return;
    if (phase === "active") tryPostToPage(END_WITHOUT_PLAYER);
    endByPageSideStep(endAfterSourceLoss(sourceLossOfExpiredWait(wait), { pageAddress: episodeChange.pageAddressSinceVideoBegan(), agreedProtocol }));
  }

  /** Конец по стороне страницы: очередь и код причины или, если одностраничник ушёл без видео, `tab_gone{navigated_away}`. */
  function endByPageSideStep(lossEnd: SourceLossEnd): void {
    if (lossEnd.kind === "end_on_page_side") return endOnPageSide(lossEnd.closeReason, lossEnd.end);
    socket?.sendTabGoneAndClose("navigated_away");
    finish({ kind: "page_gone", reason: "navigated_away" });
  }

  function heldVideo(frameId: number, report: HeldVideoReport): HeldVideoReply {
    if (phase === "page_away") return adoptReturnedVideo(frameId, report);
    if ((phase !== "active" && phase !== "between_videos") || !relocation.adoptionOpen()) return "release";
    const decision = judgeHeldVideo(frameId, report, place.fileAddressRule, playerAbilitiesOf(agreedProtocol));
    if (decision.kind === "release") return "release";
    moveToVideo(decision.video, decision.fileAddressRule, () => waitForNextVideo());
    return "adopt";
  }

  /** Сторож вернувшейся страницы задержал видео (11b2): то же видео — переезд в новую вкладку, иначе отпустить. */
  function adoptReturnedVideo(frameId: number, report: HeldVideoReport): HeldVideoReply {
    const decision = judgeHeldVideo(frameId, report, place.fileAddressRule, playerAbilitiesOf(agreedProtocol));
    if (decision.kind === "release" || !pageReturn.acceptsReturnedVideo(decision.video.durationS)) return "release";
    moveToVideo(decision.video, decision.fileAddressRule, () => pageReturn.shadowDidNotStand());
    return "adopt";
  }

  /**
   * Переезд. Прежняя тень снимается на начало ролика и на паузе: конец файла дал бы странице второе
   * `ended`. Её `handoff_ended` в фазе `moving` ничего не значит. Тот же документ — тот же порт и те же
   * локальные ключи MediaSource; другой фрейм — новый порт, старый закрывается следом за командой.
   * `ifBeginFails` — что делать, если тень не встанет: при переезде ждать следующее видео, при возврате — отказ.
   */
  function moveToVideo(video: ChosenVideo, fileAddressRule: FileAddressRule, ifBeginFails: () => void): void {
    const leavingPort = framePort;
    if (phase === "active") tryPostToPage(LEAVE_PREVIOUS_VIDEO);
    silenceWatch.stop();
    phase = "moving";
    shownRef = null;
    incomingVideo = video;
    incomingRef = { frame_id: video.frameId, document: video.document, element: video.element };
    place = { frameId: video.frameId, beginRequestId: target.nextBeginRequestId(), fileAddressRule };
    beginFallback = ifBeginFails;
    beginTimer.start(HANDOFF_BEGIN_REPLY_TIMEOUT_MS, () => {
      // Страница ответит позже, а её ответ фаза уже не примет: тень не должна остаться без хозяина.
      tryPostToPage(END_WITHOUT_PLAYER);
      ifBeginFails();
    });
    if (leavingPort === undefined || video.frameId !== portFrameId) {
      try {
        connectToFrame(video.frameId);
      } catch {
        ifBeginFails();
        return;
      }
      portFrameId = video.frameId;
      if (leavingPort !== undefined) disconnectQuietly(leavingPort);
    }
    if (tryPostToPage({ kind: "begin_handoff", request_id: place.beginRequestId, document: video.document, element: video.element }) === "port_dead") {
      ifBeginFails();
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
      else if (parseViewerReturnRequest(message) !== undefined) returnForViewer();
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
      case "next_episode_wait_began":
        // Тень ждёт следующую серию (16a). Странице не верим на слово: только если и наши часы видели конец серии.
        if (phase === "active" && relocation.episodeEnded()) episodeChange.nextEpisodeWaitBegan();
        return;
      case "file_source":
        startSourceFromPage({ form: "file", url: event.url, referrer: referrerOf(event) });
        return;
      case "hls_source":
        startSourceFromPage({ form: "hls", url: event.url, referrer: referrerOf(event), stream: event.stream });
        return;
      case "next_video_held":
        // Донесение сторожа идёт мимо порта, через пограничника: по порту его не бывает.
        return;
      case "subtitle_document":
      case "subtitle_track_source":
        // Передатчик сам отбрасывает события вне потока (`acceptsStream`) и версию плеера ниже 1.5.
        subtitles.accept(event);
        return;
      default:
        forwardStreamEvent(event);
    }
  }

  function handleHandoffBegan(event: Extract<PageEvent, { kind: "handoff_began" }>): void {
    if ((phase !== "beginning" && phase !== "moving") || event.request_id !== place.beginRequestId) return;
    beginTimer.cancel();
    if (phase === "moving" && event.outcome !== "started") {
      // Видео исчезло или стало защищённым, пока фон решал: ждём дальше (предел тот же) или, при возврате, отказ.
      beginFallback();
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
    // Тень вернувшейся страницы встала (11b2): `page_returned` — раньше любого события нового документа.
    pageReturn.shadowStood();
    pageReturn.noteVideoInPlayer({ role: incomingVideo.role, durationS: incomingVideo.durationS });
    phase = "active";
    shownRef = incomingRef;
    silenceWatch.start();
    relocation.videoStarted(incomingVideo);
    episodeChange.videoBegan();
    siteAdapters.videoStarted();
    resolveOutcome({ kind: "started", player_window: playerWindow });
  }

  function handleHandoffEnded(cause: HandoffEndCause): void {
    if (phase === "ending") {
      completeEnding();
      return;
    }
    // В `moving` это прежняя тень: мы сами её сняли. В `between_videos` тени уже нет.
    if (phase !== "active") return;
    // Тень сняла страница сама: правило — session-end-rules.ts (переезд, поломка плеера сайта, вина страницы).
    const facts = { nextVideoExpected: relocation.nextVideoExpectedAfterLoss(), pageAddress: episodeChange.pageAddressSinceVideoBegan(), agreedProtocol };
    const step = stepAfterPageEndedShadow(cause, facts);
    if (step.kind === "wait_for_next_video") waitForNextVideo();
    else if (step.kind === "leave_page") leavePage(step.reason);
    else endByPageSideStep(step);
  }

  function forwardStreamEvent(event: StreamPageEvent): void {
    if (!acceptsStream() || translator === undefined || socket === undefined) return;
    const translated = translator.translate(event);
    if (!translated.ok) {
      rejectPageStream(translated.problem);
      return;
    }
    // Новое поколение MSE после файла или HLS (находка 12 ревью 09b): загрузчик прошлого источника
    // больше не нужен — его текущие запросы обрываются, а не докачивают ненужное.
    // Новое поколение — новая серия на экране плеера (16a): опорный адрес и «ждём следующую» — заново.
    if (translated.items.some(startsGeneration)) {
      feeds.endGeneration();
      episodeChange.videoBegan();
    }
    for (const item of translated.items) socket.enqueue(item);
  }

  /** Поток страницы идёт в плеер: тень стоит, встаёт или сеанс как раз переезжает. */
  function acceptsStream(): boolean {
    return phase === "beginning" || phase === "active" || phase === "between_videos" || phase === "moving";
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
   * Страница передаёт источник, который качает фон: файл `<video src>` (09b) или нативный HLS (10a2).
   * Новое поколение, байты по запросам плеера отдают загрузчики (session-source-feeds.ts). Адрес остаётся
   * в фоне: плееру уходит номер поколения (у HLS - ещё и номерок корня).
   */
  function startSourceFromPage(source: PulledSourceEvent): void {
    if (!acceptsStream() || translator === undefined || socket === undefined) return;
    const start = { url: source.url, referrer: source.referrer, rule: place.fileAddressRule, sink: socket };
    const started = startCheckedPulledSource(feeds, translator, source, start, playerAbilitiesOf(agreedProtocol));
    switch (started.kind) {
      case "started":
        for (const item of started.items) socket.enqueue(item);
        episodeChange.videoBegan();
        return;
      case "stream_problem":
        return rejectPageStream(started.problem);
      default:
        return refuseSource(started.kind);
    }
  }

  /** Запрещённый адрес файла (решение 199) или HLS новее плеера: честный отказ, в сеть и плееру не ушло ничего. */
  function refuseSource(refusal: "address_refused" | "needs_newer_player"): void {
    const addressRefused = refusal === "address_refused";
    if (phase === "beginning") return failStart(addressRefused ? { kind: "local_address_refused" } : NATIVE_HLS_NOT_YET, SESSION_CLOSE_CODES.protocol_violation);
    endForPageFault(addressRefused ? { kind: "local_address_refused" } : { kind: "source_needs_newer_player", form: "native_hls" });
  }

  // ---------------------------------------------------------------- плеер → страница

  function handlePlayerMessage(message: PlayerSessionMessage): void {
    // Секунда плеера нужна возврату страницы во всех фазах (11b2): в сироте часы — единственная новость.
    if (message.type === "clock") pageReturn.notePlayerSecond(message.position_us);
    // Память выбора решает по часам, у начала ли серия (14b2); в сироте её слушатели и так молчат.
    if (message.type === "clock") siteAdapters.playerClock(message.position_us, message.state);
    if (phase === "page_away") {
      if (message.type === "reopen_page") pageReturn.reopenRequested();
      if (message.type === "show_page") dependencies.noteForDeveloper("show_page ignored: the page is away");
      return;
    }
    // До старта тени и после её снятия командовать некем.
    if (phase !== "active" && phase !== "between_videos" && phase !== "moving") return;
    const action = translatePlayerMessage(message, numbering.latestGeneration());
    switch (action.kind) {
      case "shadow":
        // Пока тень встаёт в вернувшейся вкладке, часы старого видео переезду не указ (он остановлен).
        if (action.command.kind === "clock" && !pageReturn.isReturning()) relocation.noteClockState(action.command.state);
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
      case "site_adapter": return void siteAdapters.command(action.command);
      case "show_page": return void pageReveal.show(); // сбои разобраны внутри, наружу не бросает
      case "fetch_resource":
        // Ресурсы HLS просят только в поколении HLS; в другом поколении фид сам ответит `refused`.
        if (socket !== undefined) feeds.requestResource(action.request, socket);
        return;
      case "fetch_range":
        // Байты файла просят только в поколении-файле. Без файла запрос не наш (плеер ошибся
        // сам): его чтение честно истечёт у плеера, сеанс страницы это не ломает.
        feeds.requestRange(action.request);
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
        endHandoff(endCommandAfterPlayerSocketEnd(cause), { kind: "player_ended", cause });
        return;
      case "between_videos":
      case "moving":
      case "page_away":
        // В сироте порта нет (команда никуда не уйдёт), а открытие страницы и нашу вкладку отменит `finish`.
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

  /** Сирота: тени, порта и загрузок нет, связь с плеером держим (11b2). Сеанс ждёт `reopen_page`. */
  function quietenForOrphan(): void {
    phase = "page_away";
    shownRef = null;
    stopPageSide();
  }

  /** Всё, что связывает сеанс со страницей: таймеры, сторожа, загрузки, порт. Связь с плеером не трогается. */
  function stopPageSide(): void {
    beginTimer.cancel();
    endTimer.cancel();
    silenceWatch.stop();
    relocation.stop();
    siteAdapters.stop();
    // Текущие загрузки источников обрываются: ни рамки после конца сеанса или ухода страницы.
    feeds.dispose();
    // Закачки субтитров страницы тоже: после конца сеанса или ухода страницы ничего не шлётся.
    subtitles.endDocument();
    if (framePort !== undefined) disconnectQuietly(framePort);
    framePort = undefined;
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
  return { outcome, notifyTabRemoved, heldVideo, adOnScreen: () => relocation.adOnScreen(), videoInPlayer: () => (phase === "active" ? shownRef : null), adWatchReport: siteAdapters.adWatchReport, variantWatchReport: siteAdapters.variantWatchReport };
}
