// Изолированный мир — «пограничник» между MAIN-миром страницы и фоном (сессия 09a).
//
// Владеет:
// - проверкой формы КАЖДОГО сообщения MAIN-мира (src/shared/page-message-validation.ts);
//   мусор отбрасывается и считается;
// - портом участка 2 (его открывает фон через tabs.connect, когда начинает передачу): пока порта нет,
//   изолированный мир ничего фону не шлёт и фон не будит;
// - запросами «опиши видео документа» (для выбора видео фоном) с таймаутом;
// - страховкой тени: если порт фона пропал посреди передачи, MAIN-мир получает `end_handoff`
//   «остаться на позиции тени, играть как тень» — страница не остаётся навсегда в тени без плеера;
// - маршрутом событий: каждое событие страницы уходит порту СВОЕЙ передачи (по номеру запроса
//   `begin_handoff`), события старой передачи в сеанс новой не попадают;
// - бюджетом событий передачи (page-event-budget.ts, решение 200): при превышении пересылка
//   останавливается сразу, а фон узнаёт об этом своим донесением `page_flood`, которое страница
//   подделать не может: оно рождается здесь, а не приходит из MAIN-мира;
// - сторожем следующего видео (сессия 09c2): фон взводит его всем фреймам вкладки, а донесение «видео
//   задержано» проходит к фону только пока взвод не истёк и только в пределах своего лимита. Иначе
//   страница могла бы будить фон сама, а решение 198 это запрещает. Нет ответа фона — видео отпускается;
// - просьбой зрителя «Return» с заглушки (сессия 11a, решение 236): она уходит фону по порту ТЕКУЩЕЙ
//   передачи и рождается здесь, а не в MAIN-мире, поэтому страница подделать её не может;
// - тем, что документ говорит о себе сам: имя хоста и происхождение читаются здесь, в изолированном
//   мире, а не берутся на веру из MAIN-мира. От них зависят проверка адреса (решение 199) и Referer
//   (решение 202), поэтому поддельное значение страницы до фона не доходит;
// - предложением «Ask» (сессия 12b): ответ фона `ask_viewer` и выбор человека ведёт `viewer-offer-relay.ts`,
//   здесь — только точка, где ответ на донесение уходит туда.
// Сам ничего не решает о сеансе: нумерация поколений, очередь и плеер — в фоне.

import type { AutoHandoffPageCommand, AutoHandoffWatchState } from "../shared/auto-handoff-contract.ts";
import {
  HELD_VIDEO_REPORT_KIND,
  NEW_SESSION_ADOPT_INSURANCE_MS,
  type ElementKey,
  PAGE_FLOOD_NOTICE_KIND,
  VIEWER_RETURN_REQUEST_KIND,
  type DescribeCandidatesQuery,
  type HeldVideoReply,
  type HeldVideoTrigger,
  type HeldVideoReport,
  type NextVideoCommand,
  type PageCommand,
  type PageEvent,
  type PageEventBudgetKind,
  type PageFloodNotice,
  type PageReturnTarget,
  type VideoCandidate,
  type ViewerReturnRequest,
} from "../shared/handoff-contract.ts";
import type { PointerQuery, PointerReply, RightClickedVideo, VideoAtPoint } from "../shared/entry-contract.ts";
import { parseRightClickedReply, parseVideoAtPointReply } from "../shared/entry-validation.ts";
import { parseCandidatesReply, parseHeldVideoReply, parsePageEvent } from "../shared/page-message-validation.ts";
import { createTokenBucket, type MillisecondClock } from "../shared/token-bucket.ts";
import { createPageEventBudget, type PageEventBudget, type PageEventBudgetParameters } from "./page-event-budget.ts";
import type { ViewerOfferChoice, ViewerOfferChoiceMessage, ViewerOfferPageCommand } from "../shared/viewer-offer-contract.ts";
import { createViewerOfferRelay, type ViewerOfferDelivery } from "./viewer-offer-relay.ts";

/**
 * Сколько ждём ответ MAIN-мира на `describe_candidates`. MAIN-мир отвечает синхронно из своего
 * обработчика, задержка — только занятость потока страницы. 1 с с запасом покрывает тяжёлую
 * страницу; фрейм без нашего MAIN-скрипта (не успел встать) честно даёт пустой список.
 */
export const DESCRIBE_CANDIDATES_TIMEOUT_MS = 1_000;

/**
 * Сколько ждём ответ MAIN-мира на вопрос про указатель (`describe_right_clicked`, `video_at_point`, 10c).
 * Ответ приходит синхронно из обработчика MAIN; без него (фрейм без нашего MAIN-скрипта) честно «видео нет».
 */
export const POINTER_QUERY_TIMEOUT_MS = 1_000;

/**
 * Сколько ждём ответ фона о задержанном видео. Обычно это миллисекунды; без ответа страница не должна
 * остаться с молчащим видео, поэтому оно отпускается. 2 с покрывают пробуждение service worker.
 */
export const HELD_VIDEO_REPLY_TIMEOUT_MS = 2_000;

/**
 * Сколько ждём ответ фона, когда видео задержано по запуску зрителя (режим 2, 12a). Фон решает асинхронно:
 * будит service worker, читает настройки из storage, спрашивает адрес вкладки (`tabs.get`) и пробует рекламу
 * (опрос около 0,6 с). 5 с покрывают это с запасом; дольше зритель смотрел бы на замершее видео.
 */
export const VIEWER_PLAY_REPLY_TIMEOUT_MS = 5_000;

/**
 * Лимит донесений о задержанных видео в секунду. Живая страница запускает одно видео за раз; 16 — с
 * запасом, а страница, которая молотит `play()` сотнями, дальше этого порога фон не тревожит.
 */
export const HELD_VIDEO_REPORTS_PER_SECOND = 16;

/** Наибольший номер запроса (u32 по договору); после него счёт идёт с 1. */
const MAX_REQUEST_ID = 0xffff_ffff;

/** Таймеры изолированного мира (в продукте — setTimeout окна, в тестах — фейковое время). */
export interface RelayTimers {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

/** Порт участка 2 (runtime.Port или фейк в тестах). */
export interface BackgroundPort {
  postMessage(message: unknown): void;
  disconnect(): void;
  readonly onMessage: { addListener(listener: (message: unknown) => void): void };
  readonly onDisconnect: { addListener(listener: () => void): void };
}

/** Счётчики пограничника — для тестов и диагностики (журнала в продукте нет). */
export interface RelayCounters {
  /** Сообщения MAIN-мира не договорной формы. */
  readonly rejectedPageMessages: number;
  /** Договорные события, которые некому отдать (нет передачи или её порт пропал). */
  readonly undeliverablePageEvents: number;
  /** Команды фона неизвестного вида. */
  readonly rejectedBackgroundMessages: number;
  /** События, отброшенные после превышения бюджета: до фона они не дошли. */
  readonly droppedFloodEvents: number;
}

export interface ContentRelay {
  /** Сообщение из MAIN-мира (уже объект этого мира, но не проверенное). */
  handlePageMessage(message: unknown): void;
  /** Фон открыл порт участка 2 к этому фрейму. */
  acceptBackgroundPort(port: BackgroundPort): void;
  /** Фон просит взвести сторож следующего видео. */
  /** Взвести сторож; `pageReturn` — цель возврата страницы (11b2), без неё — обычный переезд (09c2). */
  armNextVideo(leaseMs: number, pageReturn?: PageReturnTarget): void;
  /**
   * Режим 2 (12a): охрана включена/выключена. Запоминается здесь (донесения `viewer_play` проходят только при
   * `on`) и уходит MAIN-миру. Фону не уходит ничего: он молчит до настоящего запуска зрителя (решение 198).
   */
  setAutoHandoffWatch(state: AutoHandoffWatchState): void;
  /** Фон выдал «кредит клика» на `leaseMs` (решение 247): MAIN-мир считает запуск со звуком запуском зрителя. */
  grantPlayCredit(leaseMs: number): void;
  /** Фон просит отпустить задержанное видео (сеанс режима 2 не состоялся): MAIN-мир снимает тень. */
  releaseHeldVideo(element: ElementKey): void;
  /** Описание видео документа от MAIN-мира; таймаут или пустой фрейм — пустой список. */
  describeVideoCandidates(): Promise<readonly VideoCandidate[]>;
  /** Видео под последним настоящим правым кликом; таймаут и «ничего» — `null` (10c). */
  rightClickedVideo(): Promise<RightClickedVideo | null>;
  /**
   * Видео под точкой вьюпорта этого фрейма; таймаут и «ничего» — `null` (10c). Готово для кнопки поверх
   * видео: `in_player` вычисляет контроллер передачи в MAIN, пограничник форму проверяет.
   */
  videoAtPoint(x: number, y: number): Promise<VideoAtPoint | null>;
  /**
   * Зритель нажал «Return» на заглушке (11a): передать просьбу фону по порту текущей передачи. Звать только на
   * настоящий клик человека — это проверяет заглушка (`isTrusted`).
   */
  requestViewerReturn(): ViewerReturnDelivery;
  /** Человек нажал кнопку предложения «Ask» (12b). Звать только на настоящий клик — это проверяет панель. */
  answerViewerOffer(choice: ViewerOfferChoice): Promise<ViewerOfferDelivery>;
  /** Фон просит снять предложение с этого элемента (на вкладке встало новое). */
  withdrawViewerOffer(element: ElementKey): void;
  counters(): RelayCounters;
}

/**
 * Что стало с просьбой «Return»:
 * - `sent` — ушла фону;
 * - `no_handoff` — передачи с живым портом нет (ещё не началась, уже кончилась или фон пропал): некому;
 * - `port_failed` — порт бросил при отправке (его `onDisconnect` сам снимет тень).
 */
export type ViewerReturnDelivery = "sent" | "no_handoff" | "port_failed";

export interface RelayDependencies {
  /** Отправка MAIN-миру по личному порту участка 1. */
  readonly sendToPage: (message: PageCommand | NextVideoCommand | AutoHandoffPageCommand | ViewerOfferPageCommand | DescribeCandidatesQuery | PointerQuery) => void;
  /** Донесение фону о задержанном видео; его ответ решает судьбу видео. */
  readonly reportHeldVideo: (report: HeldVideoReport) => Promise<HeldVideoReply>;
  /** Выбор человека на предложении «Ask» фону (12b); ответ проверяет `viewer-offer-relay.ts`. */
  readonly sendViewerOfferChoice: (message: ViewerOfferChoiceMessage) => Promise<unknown>;
  readonly timers: RelayTimers;
  /** Одна строка диагностики за всю жизнь документа (первый мусор): без спама. */
  readonly debugOnce: (message: string) => void;
  /** Часы для бюджета событий передачи. */
  readonly now: MillisecondClock;
  /** Что документ знает о себе сам, прочитанное в ЭТОМ мире (страница подменить не может). */
  readonly ownDocument: () => OwnDocumentAddress;
  /** Свои лимиты бюджета (тесты); по умолчанию боевые. */
  readonly budgetLimits?: Omit<PageEventBudgetParameters, "now">;
}

/** Адрес документа глазами изолированного мира. */
export interface OwnDocumentAddress {
  /** `location.hostname`; "" — нет. */
  readonly hostname: string;
  /** `location.origin`; `null` — непрозрачное происхождение. */
  readonly origin: string | null;
}

/**
 * Передача, чей поток сейчас идёт из MAIN-мира (после `handoff_began{started}` и до `handoff_ended`).
 * MAIN-мир держит не больше одной тени, а его события упорядочены: всё между `handoff_began` и
 * `handoff_ended` относится к ОДНОЙ передаче. `port` — порт фона, который её начал; `undefined` —
 * хозяин пропал (порт закрыт или заменён): тень уже снимается, хвост событий никому не отдаётся.
 */
interface StreamOwner {
  readonly requestId: number;
  port: BackgroundPort | undefined;
  /** Бюджет именно этой передачи: следующая начинает с полного. */
  readonly budget: PageEventBudget;
  /** Бюджет кончился: пересылка остановлена, проходит только `handoff_ended`. */
  flooded: boolean;
}

/** Снять тень без плеера: остаться на позиции тени, играть/стоять как тень. */
const END_HANDOFF_WITHOUT_PLAYER: PageCommand = {
  kind: "end_handoff",
  position: { kind: "shadow_position" },
  resume: { kind: "as_shadow" },
};

interface PendingQuery {
  readonly resolve: (candidates: readonly VideoCandidate[]) => void;
  readonly timer: unknown;
}

/** Вопрос про указатель, на который MAIN-мир ещё не ответил. `expects` — какой ответ его закрывает. */
interface PendingPointerQuery {
  readonly expects: PointerReply["kind"];
  readonly resolve: (video: RightClickedVideo | VideoAtPoint | null) => void;
  readonly timer: unknown;
}

export function createContentRelay(dependencies: RelayDependencies): ContentRelay {
  const { sendToPage, timers } = dependencies;
  const pendingQueries = new Map<number, PendingQuery>();
  const pendingPointerQueries = new Map<number, PendingPointerQuery>();
  let backgroundPort: BackgroundPort | undefined;
  // Начатые фоном передачи, на которые MAIN-мир ещё не ответил: номер запроса → порт-хозяин
  // (`undefined` — хозяин пропал до ответа: если тень всё же встанет, её сразу снимаем).
  const pendingBegins = new Map<number, BackgroundPort | undefined>();
  let streamOwner: StreamOwner | undefined;
  let nextRequestId = 1;
  let rejectedPageMessages = 0;
  let undeliverablePageEvents = 0;
  let rejectedBackgroundMessages = 0;
  let droppedFloodEvents = 0;
  // Взвод сторожа следующего видео: пока он не истёк, задержанные видео доходят до фона.
  let armedUntilMs = Number.NEGATIVE_INFINITY;
  // Что изолированный мир в последний раз сказал MAIN-миру про охрану режима 2; до первой команды — `off`.
  let autoHandoffWatch: AutoHandoffWatchState = "off";
  const heldReports = createTokenBucket(HELD_VIDEO_REPORTS_PER_SECOND, dependencies.now);
  const viewerOffer = createViewerOfferRelay({
    sendToPage,
    sendChoice: dependencies.sendViewerOfferChoice,
    timers,
    note: (message) => dependencies.debugOnce(`fastiplayer bridge: ${message}`),
  });

  function rejectPageMessage(): void {
    rejectedPageMessages += 1;
    if (rejectedPageMessages === 1) dependencies.debugOnce("fastiplayer bridge: page channel message rejected");
    chargeMalformedMessage();
  }

  // ---------------------------------------------------------------- бюджет передачи

  /** Передача, чей поток ещё можно пересылать; бюджет считается только для неё. */
  function liveStreamOwner(): StreamOwner | undefined {
    return streamOwner?.port !== undefined && streamOwner.port === backgroundPort ? streamOwner : undefined;
  }

  /** Мусор во время передачи тратит её бюджет: тысячи кривых сообщений в секунду — это тоже флуд. */
  function chargeMalformedMessage(): void {
    const owner = liveStreamOwner();
    if (owner === undefined || owner.flooded) return;
    if (owner.budget.admit("malformed") === "exceeded") reportFlood(owner, "malformed");
  }

  /**
   * Бюджет кончился: сперва пересылка стоит (`flooded`), потом донесение фону. Конец сеанса и снятие
   * тени решает фон. Если донесение не ушло (порт мёртв), тень снимет `onDisconnect` порта; если
   * фон завис, он сам заметит тишину пульса: `shadow_alive` тоже больше не проходит.
   */
  function reportFlood(owner: StreamOwner, overBudget: PageEventBudgetKind): void {
    owner.flooded = true;
    const notice: PageFloodNotice = { kind: PAGE_FLOOD_NOTICE_KIND, over_budget: overBudget };
    try {
      owner.port?.postMessage(notice);
    } catch {
      undeliverablePageEvents += 1;
    }
  }

  /** Вид бюджета; `undefined` — событие бюджет не считает (`bytes`: у них потолок очереди). */
  function budgetKindOf(event: PageEvent): PageEventBudgetKind | undefined {
    switch (event.kind) {
      case "mse":
      case "page_seek":
      case "player_seek_applied":
      case "page_playback":
      case "file_source":
      case "hls_source":
      case "shadow_alive":
        return event.kind;
      default:
        return undefined;
    }
  }

  /** Событие потока: сначала бюджет, потом доставка. После флуда не проходит ничего, и байты тоже. */
  function forwardStreamEvent(event: PageEvent): void {
    const owner = liveStreamOwner();
    if (owner === undefined) {
      deliver(undefined, event);
      return;
    }
    if (owner.flooded) {
      droppedFloodEvents += 1;
      return;
    }
    const budgetKind = budgetKindOf(event);
    if (budgetKind !== undefined && owner.budget.admit(budgetKind) === "exceeded") {
      droppedFloodEvents += 1;
      reportFlood(owner, budgetKind);
      return;
    }
    deliver(owner.port, withOwnDocumentFacts(event));
  }

  /** Заменить то, что страница сказала о своём документе, на прочитанное в этом мире. */
  function withOwnDocumentFacts(event: PageEvent): PageEvent {
    // `hls_source` несёт те же факты Referer, что `file_source`: происхождение документа берём своё, не страницы.
    if ((event.kind !== "file_source" && event.kind !== "hls_source") || event.referrer_origin === null) return event;
    return { ...event, referrer_origin: dependencies.ownDocument().origin };
  }

  // ---------------------------------------------------------------- «Return» зрителя (11a)

  function requestViewerReturn(): ViewerReturnDelivery {
    // Только хозяину текущей передачи: просьба относится к той тени, над которой стоит заглушка.
    const owner = streamOwner;
    if (owner === undefined || owner.port === undefined || owner.port !== backgroundPort) return "no_handoff";
    const request: ViewerReturnRequest = { kind: VIEWER_RETURN_REQUEST_KIND };
    try {
      owner.port.postMessage(request);
      return "sent";
    } catch {
      return "port_failed";
    }
  }

  // ---------------------------------------------------------------- MAIN → фон

  function handlePageMessage(message: unknown): void {
    const pointerReply = parseRightClickedReply(message) ?? parseVideoAtPointReply(message);
    if (pointerReply !== undefined) {
      // Как и с кандидатами: ответ на незаданный вопрос — подделка или опоздание, тратит бюджет передачи.
      if (!settlePointerQuery(pointerReply)) chargeMalformedMessage();
      return;
    }
    const reply = parseCandidatesReply(message);
    if (reply !== undefined) {
      // Ответ на незаданный вопрос — опоздание, но страница может сыпать ими так же, как мусором.
      if (!settleQuery(reply.request_id, reply.candidates)) chargeMalformedMessage();
      return;
    }
    const event = parsePageEvent(message);
    if (event === undefined) {
      rejectPageMessage();
      return;
    }
    routePageEvent(event);
  }

  /**
   * Событие страницы уходит ТОЛЬКО порту той передачи, к которой оно относится. Так хвост старой
   * передачи (между нашим `end_handoff` и её `handoff_ended`) не попадает в сеанс новой, а фаза
   * новой передачи не сбивается событиями старой.
   */
  function routePageEvent(event: PageEvent): void {
    switch (event.kind) {
      case "handoff_began": {
        if (!pendingBegins.has(event.request_id)) {
          // Ответ на запрос, которого фон не делал (подделка страницы или давно забытый): некому.
          undeliverablePageEvents += 1;
          return;
        }
        const owner = pendingBegins.get(event.request_id);
        pendingBegins.delete(event.request_id);
        const ownerIsAlive = owner !== undefined && owner === backgroundPort;
        if (event.outcome === "started") {
          streamOwner = {
            requestId: event.request_id,
            port: ownerIsAlive ? owner : undefined,
            budget: createPageEventBudget({ now: dependencies.now, ...dependencies.budgetLimits }),
            flooded: false,
          };
          // Хозяин пропал, пока MAIN-мир начинал: тень без плеера сразу снимаем.
          if (!ownerIsAlive) sendToPage(END_HANDOFF_WITHOUT_PLAYER);
        }
        deliver(ownerIsAlive ? owner : undefined, event);
        return;
      }
      case "handoff_ended": {
        const owner = streamOwner?.port;
        streamOwner = undefined;
        deliver(owner, event);
        return;
      }
      case "next_video_held":
        reportHeldVideo(event);
        return;
      default:
        forwardStreamEvent(event);
    }
  }

  function deliver(port: BackgroundPort | undefined, event: PageEvent): void {
    if (port === undefined || port !== backgroundPort) {
      undeliverablePageEvents += 1;
      return;
    }
    try {
      port.postMessage(event.kind === "bytes" ? { ...event, bytes: exactBytes(event.bytes) } : event);
    } catch {
      // Порт уже мёртв: его onDisconnect придёт следом и снимет тень; событие отдать некому.
      undeliverablePageEvents += 1;
    }
  }

  // ---------------------------------------------------------------- сторож следующего видео

  /** `pageReturn` — цель взвода возврата страницы (11b2); без неё — обычный взвод переезда (09c2). */
  function armNextVideo(leaseMs: number, pageReturn?: PageReturnTarget): void {
    armedUntilMs = dependencies.now() + leaseMs;
    sendToPage(pageReturn === undefined ? { kind: "arm_next_video", lease_ms: leaseMs } : { kind: "arm_next_video", lease_ms: leaseMs, page_return: pageReturn });
  }

  function setAutoHandoffWatch(state: AutoHandoffWatchState): void {
    autoHandoffWatch = state;
    sendToPage({ kind: "auto_handoff_watch", state });
  }

  function grantPlayCredit(leaseMs: number): void {
    sendToPage({ kind: "grant_play_credit", lease_ms: leaseMs });
  }

  function releaseHeldVideo(element: ElementKey): void {
    sendToPage({ kind: "release_held_video", element });
  }

  /**
   * Можно ли донести о задержанном видео. `next_video` — только пока взвод фона жив; `viewer_play` — только
   * пока мы сами включили охрану (`on`). Иначе это страница, а не наш сторож. Лимит частоты общий.
   */
  function mayReportHeldVideo(trigger: HeldVideoTrigger): boolean {
    const triggerIsAllowed = trigger === "viewer_play" ? autoHandoffWatch !== "off" : dependencies.now() < armedUntilMs;
    return triggerIsAllowed && heldReports.take(1);
  }

  /**
   * Сторож задержал видео. Вне разрешения или сверх лимита донесение до фона не идёт: это страница, а не наш
   * сторож, или страница молотит `play()`. Видео отпускается, и она сама получает то, что просила.
   */
  function reportHeldVideo(event: Extract<PageEvent, { kind: "next_video_held" }>): void {
    const release = (): void => releaseHeldVideo(event.element);
    if (!mayReportHeldVideo(event.trigger)) {
      release();
      return;
    }
    let settled = false;
    // Только явное «берём» оставляет видео под тенью; ответ не из договора (или его отсутствие) — отпустить.
    const settle = (reply: HeldVideoReply | undefined): void => {
      if (settled) return;
      settled = true;
      timers.clearTimeout(timer);
      if (reply === undefined || reply === "release") {
        release();
        return;
      }
      // Сайт `ask` (12b): видео держится под предложением без срока — выбор за человеком (решение 256).
      // Спросить можно только о запуске зрителя: другое задержанное видео сторож под предложение не возьмёт и
      // осталось бы под тенью навсегда.
      if (reply === "ask_viewer") {
        const document = event.candidates.find((candidate) => candidate.element === event.element)?.document;
        if (document === undefined || event.trigger !== "viewer_play") release();
        else viewerOffer.offerStood(event.element, document);
        return;
      }
      // Фон взял видео, но мог умереть до `begin_handoff`. Забранное видео этой командой не отпустить:
      // для него она ничего не значит, поэтому страховка ставится всегда. Новый сеанс (режим 2) стартует долго.
      timers.setTimeout(release, reply === "adopt_new_session" ? NEW_SESSION_ADOPT_INSURANCE_MS : HELD_VIDEO_REPLY_TIMEOUT_MS);
    };
    const replyTimeoutMs = event.trigger === "viewer_play" ? VIEWER_PLAY_REPLY_TIMEOUT_MS : HELD_VIDEO_REPLY_TIMEOUT_MS;
    const timer = timers.setTimeout(() => settle("release"), replyTimeoutMs);
    dependencies
      .reportHeldVideo({ kind: HELD_VIDEO_REPORT_KIND, element: event.element, candidates: event.candidates.map(withOwnHostName), trigger: event.trigger })
      .then((reply) => settle(parseHeldVideoReply(reply)), () => settle("release"));
  }

  // ---------------------------------------------------------------- фон → MAIN

  function acceptBackgroundPort(port: BackgroundPort): void {
    // Новый порт заменяет старый: старый закрываем так же, как если бы он оборвался.
    if (backgroundPort !== undefined) {
      const previous = backgroundPort;
      handleBackgroundGone(previous);
      safeDisconnect(previous);
    }
    backgroundPort = port;
    port.onMessage.addListener((message) => {
      if (backgroundPort === port) handleBackgroundCommand(port, message);
    });
    port.onDisconnect.addListener(() => handleBackgroundGone(port));
  }

  function handleBackgroundCommand(port: BackgroundPort, message: unknown): void {
    if (!isPageCommand(message)) {
      rejectedBackgroundMessages += 1;
      return;
    }
    if (message.kind === "begin_handoff") pendingBegins.set(message.request_id, port);
    sendToPage(message);
  }

  function handleBackgroundGone(port: BackgroundPort): void {
    if (backgroundPort !== port) return;
    backgroundPort = undefined;
    // Тень этого хозяина снимаем; её хвост до handoff_ended уже никому не отдаётся.
    if (streamOwner !== undefined && streamOwner.port === port) {
      streamOwner.port = undefined;
      sendToPage(END_HANDOFF_WITHOUT_PLAYER);
    }
    for (const [requestId, owner] of pendingBegins) if (owner === port) pendingBegins.set(requestId, undefined);
  }

  // ---------------------------------------------------------------- кандидаты видео

  function describeVideoCandidates(): Promise<readonly VideoCandidate[]> {
    const requestId = takeRequestId();
    return new Promise((resolve) => {
      const timer = timers.setTimeout(() => void settleQuery(requestId, []), DESCRIBE_CANDIDATES_TIMEOUT_MS);
      pendingQueries.set(requestId, { resolve, timer });
      sendToPage({ kind: "describe_candidates", request_id: requestId });
    });
  }

  // ---------------------------------------------------------------- указатель (10c)

  function takeRequestId(): number {
    const requestId = nextRequestId;
    nextRequestId = nextRequestId >= MAX_REQUEST_ID ? 1 : nextRequestId + 1;
    return requestId;
  }

  /** Задать MAIN-миру вопрос про указатель; ответ закрывает его, молчание за срок — `null`. */
  function askPointer<Video extends RightClickedVideo | VideoAtPoint>(
    expects: PointerReply["kind"],
    buildQuery: (requestId: number) => PointerQuery,
  ): Promise<Video | null> {
    const requestId = takeRequestId();
    return new Promise((resolve) => {
      const timer = timers.setTimeout(() => void settlePointerAnswer(requestId, null), POINTER_QUERY_TIMEOUT_MS);
      pendingPointerQueries.set(requestId, { expects, resolve: (video) => resolve(video as Video | null), timer });
      sendToPage(buildQuery(requestId));
    });
  }

  function settlePointerAnswer(requestId: number, video: RightClickedVideo | VideoAtPoint | null): void {
    const pending = pendingPointerQueries.get(requestId);
    if (pending === undefined) return;
    pendingPointerQueries.delete(requestId);
    timers.clearTimeout(pending.timer);
    pending.resolve(video);
  }

  /** `false` — ответ на незаданный вопрос или на вопрос другого вида. */
  function settlePointerQuery(reply: PointerReply): boolean {
    const pending = pendingPointerQueries.get(reply.request_id);
    if (pending === undefined || pending.expects !== reply.kind) return false;
    settlePointerAnswer(reply.request_id, reply.video);
    return true;
  }

  const rightClickedVideo = (): Promise<RightClickedVideo | null> =>
    askPointer("right_clicked", (requestId) => ({ kind: "describe_right_clicked", request_id: requestId }));

  const videoAtPoint = (x: number, y: number): Promise<VideoAtPoint | null> =>
    askPointer("video_at_point_result", (requestId) => ({ kind: "video_at_point", request_id: requestId, x, y }));

  /** Кандидат с именем хоста, прочитанным здесь: от него зависит проверка адреса файла (решение 199). */
  function withOwnHostName(candidate: VideoCandidate): VideoCandidate {
    return { ...candidate, frame_host: dependencies.ownDocument().hostname };
  }

  /** Отдать ответ ждущему; `false` — ответ на незаданный или просроченный запрос (не мусор формы). */
  function settleQuery(requestId: number, candidates: readonly VideoCandidate[]): boolean {
    const pending = pendingQueries.get(requestId);
    if (pending === undefined) return false;
    pendingQueries.delete(requestId);
    timers.clearTimeout(pending.timer);
    pending.resolve(candidates.map(withOwnHostName));
    return true;
  }

  return {
    handlePageMessage,
    acceptBackgroundPort,
    armNextVideo,
    setAutoHandoffWatch,
    grantPlayCredit,
    releaseHeldVideo,
    describeVideoCandidates,
    rightClickedVideo,
    videoAtPoint,
    requestViewerReturn,
    answerViewerOffer: (choice) => viewerOffer.answer(choice),
    withdrawViewerOffer: (element) => viewerOffer.withdraw(element),
    counters: () => ({ rejectedPageMessages, undeliverablePageEvents, rejectedBackgroundMessages, droppedFloodEvents }),
  };
}

/**
 * Байты ровно своего размера. Структурное клонирование `Uint8Array`-вида копирует ВЕСЬ его буфер
 * (разведка 09a: вид 1 МиБ на 16 МиБ буфере уходит в фон 16 МиБ). Если MAIN-мир прислал вид,
 * а не точную копию, режем здесь: копируются только нужные байты.
 */
export function exactBytes(bytes: Uint8Array): Uint8Array {
  const isExact = bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength;
  return isExact ? bytes : bytes.slice();
}

/** Команда фона известного вида (фон — свой, поэтому без глубокой проверки полей). */
function isPageCommand(message: unknown): message is PageCommand {
  if (typeof message !== "object" || message === null) return false;
  const kind = (message as { kind?: unknown }).kind;
  return kind === "begin_handoff" || kind === "shadow" || kind === "end_handoff";
}

function safeDisconnect(port: BackgroundPort): void {
  try {
    port.disconnect();
  } catch {
    // Порт уже закрыт браузером — освобождать нечего.
  }
}
