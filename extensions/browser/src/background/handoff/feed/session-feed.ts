// Края ленты коротких видео для одного сеанса передачи (сессия 16b3, правила F2, F3, F5).
//
// Владеет фактами «сеанса-ленты», которых нет ни у переезда (video-relocation.ts), ни у адреса серии
// (episode-change/page-address-tracker.ts):
// - был ли у сеанса хоть один шаг ленты (переезд, принятый, пока лента листалась);
// - как ушёл с экрана текущий ролик: прокруткой (элемент в документе) или страница вынула элемент (16b3, F4);
// - раздел ленты (feed-section.ts) — ВЫУЧЕННЫЙ на шаге ленты, — и последний раздел вкладки;
// - когда вкладка в последний раз сменила раздел и когда сеанс-лента потерял ролик без ухода прокруткой;
// - отсрочку конца по смене раздела (F5).
// Сеанс (handoff-session.ts, у предела размера) только сообщает сюда события и спрашивает решения. Фазой, сокетом
// и концом сеанса владеет он: модуль просит конец через `leaveFeed`.
//
// Правила:
// - F2. Сеанс-лента (был шаг ленты или лента листается сейчас) потерял текущий ролик (`handoff_ended
//   {element_released}`) — лента продолжается: ждать её следующий ролик без предела, а не 4009. До первого шага
//   и без ухода с экрана — прежнее правило (`not_feed`: 4009 «The page took the video back» или уход по адресу).
// - F3. Уход из ленты: ролик потерян, НЕ уйдя перед этим прокруткой, и вкладка сменила РАЗДЕЛ (feed-section.ts)
//   после старта ролика, в пределах окна `FEED_EXIT_WINDOW_MS` — в любом порядке. Раздел раньше потери — ответ
//   `left_feed` сразу; потеря раньше раздела — сеанс сперва ждёт, а пришедший в окне адрес другого раздела просит
//   конец через `leaveFeed`. Конец — `tab_gone{navigated_away}` («Source tab closed», досмотр скачанного; решения
//   42, 306).
//   Почему раздел, а не любой адрес (отступление от буквы правила, отчёт 16b3): Instagram меняет адрес на КАЖДОМ
//   ролике (I1, I4). В быстрой серии промежуточный ролик встаёт, адрес уходит на следующий, а элемент удаляют;
//   его уход с экрана мог не дойти (пауза пришла, пока ролик ещё был задержан сторожем). По букве «любой адрес» это
//   «Source tab closed» посреди листания, а должно быть ожидание следующего ролика (F2). Цена: «Назад» в истории
//   на /reels/ (I7, лента строится заново) — не конец, а ожидание нового ролика той же ленты.
// - F5. Пока сеанс ждёт следующий ролик ленты, вкладка ушла в ДРУГОЙ раздел (адрес без идентификаторов роликов
//   другой): через `FEED_SECTION_LEAVE_GRACE_MS` — честный конец, если раздел не вернулся и ролик не встал. Ролик,
//   задержанный в другом разделе, переезд не берёт (`heldVideoPlace` → `other_section`): тихое превью
//   «Интересного» или главной окно не уводит.
//
// Раздел ленты выучивается, а не берётся из адреса при старте (правило владельца: неверный конец хуже ожидания).
// Страница может открыться не на адресе ленты (стенд: `/…/feed.html`, первый шаг — `pushState('/reels/2/')`;
// Instagram: `/reels/` до первого `/reels/<id>/`). Поэтому раздел запоминается, когда встаёт ролик ШАГА ЛЕНТЫ, и
// уточняется новостью об адресе, пришедшей в `FEED_SECTION_LEARN_MS` после этого, пока ролик не уходил с экрана
// (новость `tabs.onUpdated` о `pushState` шага может прийти позже, чем встала тень). До первого шага ленты раздела
// нет: F5 молчит, ролик «другого раздела» не отсеивается, F3 не судит — работают прежние правила.
//
// Время — монотонные часы фона (`Scheduler.now`), отсрочка — таймер фона: таймеры скрытой вкладки спят.

import type { ScreenExitKind } from "../../../shared/handoff-contract.ts";
import type { Scheduler } from "../../host-link/native-port.ts";
import { createTimerSlot } from "../../host-link/timer-slot.ts";
import { feedSectionOf, type FeedSection } from "./feed-section.ts";

/**
 * Через сколько после смены адреса сайт успевает вынуть ролик: наблюдение 19 ноября 2026 — Instagram ~0,5 с,
 * TikTok 6 мс. Вдвое с запасом.
 */
export const SITE_REMOVAL_AFTER_ADDRESS_MS = 1_000;

/**
 * Допуск тени «элемент вне документа» (page/shadow/detached-element-watch.ts `DETACHED_ELEMENT_GRACE_MS`):
 * `handoff_ended{element_released}` приходит не раньше, чем через него после удаления. Число повторено здесь,
 * потому что фон не импортирует модули MAIN-мира; совпадение держит тест.
 */
export const PAGE_DETACHED_GRACE_MS = 1_000;

/**
 * Тень замечает удаление на тике часов плеера (раз в 250 мс), плюс путь события через изолированный мир и порт.
 * Секунда — с запасом на занятую вкладку.
 */
export const CLOCK_TICK_ALLOWANCE_MS = 1_000;

/**
 * Окно правила F3 «адрес сменился и ролик потерян» в часах фона: сайт вынимает ролик, тень замечает это через
 * свой допуск на ближайшем тике. Снятый источник (`emptied`, Shorts) тень ждёт 5 с — дольше окна: такой уход
 * решает прежнее правило «адрес сменился с начала видео» (сеанс Shorts без переездов) или отсрочка F5.
 */
export const FEED_EXIT_WINDOW_MS = SITE_REMOVAL_AFTER_ADDRESS_MS + PAGE_DETACHED_GRACE_MS + CLOCK_TICK_ALLOWANCE_MS;

/**
 * Отсрочка конца по смене раздела (F5). Новости об адресе идут через `tabs.onUpdated`, а события страницы — через
 * порт: ролик того же раздела, задержанный за миг до новости, должен успеть встать, а сайт, на мгновение
 * переписавший адрес, — вернуть его. Дольше ждать незачем: в новом разделе через 2–3 с играет своё превью.
 */
export const FEED_SECTION_LEAVE_GRACE_MS = 1_000;

/**
 * Сколько после того, как встал ролик шага ленты, новость об адресе ещё считается адресом этого шага, а не уходом.
 * Сайт меняет адрес вместе с шагом (I1: `pushState` за 80–400 мс до `play()` следующего), а новость браузера идёт
 * своим путём. Уход в другой раздел в эту секунду после шага выучится как раздел ленты: окно будет ждать, а не
 * кончится — неверный конец хуже ожидания.
 */
export const FEED_SECTION_LEARN_MS = 1_000;

/** Что значит потеря текущего ролика для ленты. */
export type FeedLoss =
  /** Сеанс — не лента: решает прежнее правило (4009 или уход одностраничника по адресу). */
  | "not_feed"
  /** Лента продолжается (F2): ждать следующий ролик без предела. */
  | "feed_continues"
  /** Уход из ленты (F3): `tab_gone{navigated_away}` сразу. */
  | "left_feed";

/** Где задержано видео, на которое сеанс может переехать. */
export type HeldVideoPlace =
  /** Сеанс не ждёт ролик ленты: раздел не судится (реклама, серия, перезагрузка). */
  | "not_a_feed_move"
  /** Тот же раздел, что у ленты. */
  | "feed_section"
  /** Вкладка уже в другом разделе (F5): переезд не берёт. */
  | "other_section";

/** Ответ сеанса на просьбу кончиться уходом из ленты. */
export type LeaveFeedAnswer =
  /** Сеанс кончен (или уже кончался). */
  | "ended"
  /** Сеанс прямо сейчас переезжает: спросить снова после отсрочки. */
  | "not_now";

export interface SessionFeedPorts {
  readonly scheduler: Scheduler;
  /** Адрес вкладки при старте сеанса (`HandoffTarget.tabUrl`), если браузер его назвал. */
  readonly openedUrl: string | undefined;
  /** Ждёт ли переезд следующий ролик ленты (`VideoRelocation.waitsForNextFeedVideo`). */
  readonly feedAwaitsNextVideo: () => boolean;
  /** Уход из ленты доказан: сеанс кончается `tab_gone{navigated_away}`. */
  readonly leaveFeed: () => LeaveFeedAnswer;
}

export interface SessionFeed {
  /** Браузер сообщил адрес главного документа вкладки сеанса (повтор того же адреса — не смена). */
  addressSeen(url: string): void;
  /** Сеанс принял задержанное видео и начал переезд на него (до `handoff_began`). */
  moveToHeldVideoBegan(): void;
  /** Тень встала на видео (первое в сеансе или после переезда): факты текущего ролика — заново. */
  videoStarted(): void;
  /**
   * Тень донесла уход ролика с экрана (`video_left_screen{exit}`). Зовётся ПОСЛЕ того, как переезд узнал об уходе
   * (`VideoRelocation.ourVideoLeftScreen`): отсрочка F5 спрашивает переезд, ждёт ли он ролик ленты.
   */
  videoLeftScreen(exit: ScreenExitKind): void;
  /** Страница сама забрала текущий ролик (`handoff_ended{element_released}`): что это значит для ленты. */
  currentVideoLost(): FeedLoss;
  /** Где задержано видео: в разделе ленты или уже в другом (F5). */
  heldVideoPlace(): HeldVideoPlace;
  /**
   * Плееру ушло слово страницы об игре (`page_playback{play|pause}`): от тени или от самого сеанса, когда он встал
   * ждать следующий ролик. Скорость (`set_rate`) игру не меняет.
   */
  pagePlaybackForwarded(request: "play" | "pause" | "set_rate"): void;
  /**
   * Окно стоит на паузе по слову страницы (а не зрителя). При уходе из ленты такую паузу нужно снять: сайт ставит
   * уходящий ролик на паузу сам (I6, T5), сеанс — пока ждал следующий ролик, а уход — это «досмотреть скачанное»
   * (решения 42, 306). Паузу зрителя страница не повторяет, и она остаётся: окно ждёт человека.
   */
  windowPausedByPage(): boolean;
  /** Сеанс кончился: снять отсрочку; новости после этого ничего не делают. */
  stop(): void;
}

export function createSessionFeed(ports: SessionFeedPorts): SessionFeed {
  const leaveTimer = createTimerSlot(ports.scheduler);
  let stopped = false;
  let latestUrl = ports.openedUrl;
  let latestSection = latestUrl === undefined ? undefined : feedSectionOf(latestUrl);
  // Когда вкладка в последний раз сменила раздел (адрес того же раздела — шаг Instagram — не в счёт).
  let lastSectionChangeAtMs: number | undefined;
  // Когда встал текущий ролик: раздел, сменённый раньше, — уже раздел этого ролика, не уход из него.
  let videoStartedAtMs = Number.NEGATIVE_INFINITY;
  // Выученный раздел ленты (`undefined` — шага ленты ещё не было): с ним сравнивается адрес (F3, F5).
  let feedSection: FeedSection | undefined;
  // До какого мига новость об адресе уточняет выученный раздел (см. шапку).
  let sectionLearningUntilMs = Number.NEGATIVE_INFINITY;
  // Хоть один переезд сеанса был шагом ленты (F2).
  let feedStepTaken = false;
  // Идущий переезд начат, пока лента листалась: станет шагом ленты, когда тень встанет.
  let feedMovePending = false;
  // Как ушёл с экрана текущий ролик (`undefined` — не уходил).
  let currentExit: ScreenExitKind | undefined;
  // Текущий ролик потерян, а лента продолжается: ждём следующий (F2), даже если переезд ещё не знает.
  let lostAndWaiting = false;
  // Ролик потерян без ухода прокруткой: адрес, пришедший в окне после этого, — уход из ленты (F3).
  let unexplainedLossAtMs: number | undefined;
  // Адрес пришёл в окне после такой потери: конец просится, пока сеанс его не примет.
  let leaveRequested = false;
  // Последнее слово страницы об игре, ушедшее плееру (`undefined` — не было: окно играет, как после передачи).
  let lastPagePlayback: "play" | "pause" | undefined;

  const now = (): number => ports.scheduler.now();

  /** Ждёт ли сеанс следующий ролик ленты. */
  function waitingForFeedVideo(): boolean {
    return lostAndWaiting || ports.feedAwaitsNextVideo();
  }

  /** Вкладка ушла в другой раздел, чем у текущего ролика (неразборный или неизвестный адрес — не ушла). */
  function sectionChanged(): boolean {
    return feedSection !== undefined && latestSection !== undefined && latestSection !== feedSection;
  }

  /** Пора ли просить конец: ждём ролик ленты, а вкладка в другом разделе (F5) или адрес пришёл после потери (F3). */
  function leaveIsDue(): boolean {
    return waitingForFeedVideo() && (sectionChanged() || leaveRequested);
  }

  /** Попросить конец; сеанс посреди переезда — спросить снова после отсрочки, если повод не прошёл. */
  function requestLeave(): void {
    leaveTimer.cancel();
    if (ports.leaveFeed() === "not_now") leaveTimer.start(FEED_SECTION_LEAVE_GRACE_MS, recheckLeave);
  }

  function recheckLeave(): void {
    if (!stopped && leaveIsDue()) requestLeave();
  }

  /** Отсрочка F5: повод есть — начать (идущую не продлевать), повода нет — снять. */
  function reviewSection(): void {
    if (stopped) return;
    if (!leaveIsDue()) leaveTimer.cancel();
    else if (!leaveTimer.isActive()) leaveTimer.start(FEED_SECTION_LEAVE_GRACE_MS, recheckLeave);
  }

  /** Вкладка ушла в другой раздел ПОСЛЕ старта текущего ролика, не дальше окна F3 от этого мига, и не вернулась. */
  function sectionLeftWithinExitWindow(): boolean {
    if (lastSectionChangeAtMs === undefined || lastSectionChangeAtMs < videoStartedAtMs) return false;
    return sectionChanged() && now() - lastSectionChangeAtMs <= FEED_EXIT_WINDOW_MS;
  }

  return {
    addressSeen(url) {
      if (stopped || url === latestUrl) return;
      latestUrl = url;
      const section = feedSectionOf(url);
      if (section !== latestSection) lastSectionChangeAtMs = now();
      latestSection = section;
      // Адрес самого шага ленты, пришедший чуть позже тени, — это раздел ленты, а не уход из него.
      if (now() <= sectionLearningUntilMs && currentExit === undefined) feedSection = section;
      // Порядок «удаление → адрес» (F3): ролик уже потерян без прокрутки, другой раздел пришёл в окне — уход сразу.
      if (unexplainedLossAtMs !== undefined && sectionChanged() && now() - unexplainedLossAtMs <= FEED_EXIT_WINDOW_MS) {
        leaveRequested = true;
        requestLeave();
        return;
      }
      reviewSection();
    },
    moveToHeldVideoBegan() {
      feedMovePending = ports.feedAwaitsNextVideo();
    },
    videoStarted() {
      if (feedMovePending) {
        // Шаг ленты: раздел вкладки сейчас и есть раздел ленты (см. шапку).
        feedStepTaken = true;
        feedSection = latestSection;
        sectionLearningUntilMs = now() + FEED_SECTION_LEARN_MS;
      }
      feedMovePending = false;
      currentExit = undefined;
      lostAndWaiting = false;
      unexplainedLossAtMs = undefined;
      leaveRequested = false;
      videoStartedAtMs = now();
      // Новое видео: его игру страница назовёт сама (`page_playback` сразу за `handoff_began`), прежнее слово не в счёт.
      lastPagePlayback = undefined;
      leaveTimer.cancel();
    },
    videoLeftScreen(exit) {
      currentExit = exit;
      reviewSection();
    },
    currentVideoLost() {
      if (stopped || !(feedStepTaken || ports.feedAwaitsNextVideo())) return "not_feed";
      const scrolledAway = currentExit === "scrolled_off_screen";
      if (!scrolledAway && sectionLeftWithinExitWindow()) return "left_feed";
      if (!scrolledAway) unexplainedLossAtMs = now();
      lostAndWaiting = true;
      reviewSection();
      return "feed_continues";
    },
    heldVideoPlace() {
      if (!waitingForFeedVideo()) return "not_a_feed_move";
      return sectionChanged() ? "other_section" : "feed_section";
    },
    pagePlaybackForwarded(request) {
      if (request !== "set_rate") lastPagePlayback = request;
    },
    windowPausedByPage: () => lastPagePlayback === "pause",
    stop() {
      stopped = true;
      leaveTimer.cancel();
    },
  };
}
