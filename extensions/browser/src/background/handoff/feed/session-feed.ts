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
//   конец через `leaveFeed`. Конец — `tab_gone{navigated_away}` («Source tab closed»; решения 42, 306; окно встаёт
//   на паузу — решение 370).
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
// - Уход до первого шага ленты (стенд `feed-leave-before-step-*`): раздел ещё не выучен, но уход сайта виден и
//   без него. Страница ВЫНУЛА текущий ролик (уход `removed_from_document`, не прокрутка), а адрес вкладки с начала
//   этого ролика сменился — в любом порядке, второе в пределах `FEED_EXIT_WINDOW_MS` от первого. Так уходят
//   Instagram (I6: адрес, удаление, пауза уже вынутого) и TikTok (T5). Шаг ленты так не выглядит: TikTok вынимает
//   ролик без смены адреса (T1), Instagram уходит прокруткой (I1), Shorts переносит элемент без ухода с экрана (S1).
//   Пока конец не принят (сеанс посреди переезда), задержанное видео не берётся: это превью нового раздела.
//
// Лента одного элемента (16b3c, решения 374, 381, 382; список — single-element-feeds.ts). YouTube Shorts листается
// новым поколением ТОГО ЖЕ элемента: переездов нет, и выученного раздела не бывает. Такую ленту сеанс узнаёт только
// по адресу из списка (адрес открытия или любая новость об адресе — порядок «поколение раньше адреса» не важен):
// - 382. Страница убрала ролик (`element_released`), адрес всё ещё в ленте — лента продолжается (`feed_continues`,
//   ожидание без предела, как F2), а не 4009. Правило «уход до первого шага» для такой ленты молчит: адрес Shorts
//   меняется на каждом ролике, и «адрес сменился с начала ролика» здесь — шаг ленты, а не уход.
// - 381. Адрес вкладки ушёл из ленты списка — через `FEED_SECTION_LEAVE_GRACE_MS` конец уходом из ленты
//   (`leaveFeed`: окно на паузе, «Source tab closed», решение 370), даже если ролик не терялся и ничего не ждём:
//   `element_released` (5 с тени после снятого источника) не ждётся.
// - Превью нового раздела окно не берёт: задержанное видео — «другой раздел», а новое поколение страницы, пришедшее
//   уже вне ленты (новый MediaSource на том же элементе), — повод кончить сеанс сразу, ДО того как поколение уйдёт
//   плееру (`pageGenerationBegan`). Поколение, пришедшее раньше новости об адресе, неотличимо от шага ленты: окно
//   его покажет, а конец придёт по адресу через отсрочку.
// Сериал с номером серии в адресе (`/serial/123/4`) в список не входит: его новое поколение и смена адреса судятся
// прежними правилами (16a, 306) — никогда как лента.
//
// Время — монотонные часы фона (`Scheduler.now`), отсрочка — таймер фона: таймеры скрытой вкладки спят.

import type { ScreenExitKind } from "../../../shared/handoff-contract.ts";
import type { Scheduler } from "../../host-link/native-port.ts";
import { createTimerSlot } from "../../host-link/timer-slot.ts";
import { feedSectionOf, type FeedSection } from "./feed-section.ts";
import { createSingleElementFeedWatch } from "./single-element-feeds.ts";

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
  /** Вкладка уже в другом разделе (F5) или уход из ленты уже доказан: переезд не берёт. */
  | "other_section";

/**
 * Откуда сеанс ушёл из ленты (16b3b, решение 375): туда «Назад» вернёт ролик в то же окно.
 * - `not_left` — сеанс не кончался уходом из ленты (или раздел неизвестен: браузер не назвал адрес);
 * - `left` — кончился, `section` — раздел ленты: выученный на шаге, а до первого шага — раздел адреса, на котором
 *   встал текущий ролик.
 */
export type FeedLeftPlace = { readonly kind: "not_left" } | { readonly kind: "left"; readonly section: FeedSection };

const NOT_LEFT: FeedLeftPlace = { kind: "not_left" };

/** Ответ сеанса на просьбу кончиться уходом из ленты. */
export type LeaveFeedAnswer =
  /** Сеанс кончен (или уже кончался). */
  | "ended"
  /** Сеанс прямо сейчас переезжает: спросить снова после отсрочки. */
  | "not_now";

/** Что делать сеансу с новым поколением страницы (`generation_started` в переводе её события). */
export type PageGenerationAnswer =
  /** Отдать плееру, как всегда: шаг ленты, новая серия, первое видео сеанса. */
  | "forward"
  /**
   * Вкладка уже ушла из ленты одного элемента, и сеанс кончился уходом из ленты прямо сейчас (381): поколение — это
   * превью нового раздела, плееру его не отдавать.
   */
  | "session_ended";

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
   * Страница начала новое поколение (новый MediaSource; у Shorts так выглядит и шаг ленты, и превью главной на том
   * же элементе). Зовётся ДО того, как поколение уйдёт плееру. Вне ленты одного элемента (381) просит конец сразу.
   */
  pageGenerationBegan(): PageGenerationAnswer;
  /**
   * Плееру ушло слово страницы об игре (`page_playback{play|pause}`): от тени или от самого сеанса, когда он встал
   * ждать следующий ролик. Скорость (`set_rate`) игру не меняет.
   */
  pagePlaybackForwarded(request: "play" | "pause" | "set_rate"): void;
  /**
   * Окно уже стоит на паузе по слову страницы: сайт сам ставит уходящий ролик на паузу (I6, T5), сеанс — пока ждёт
   * следующий ролик. При уходе из ленты окно должно стоять на паузе (решение 370): если пауза уже есть, второй не
   * нужно.
   */
  windowPausedByPage(): boolean;
  /** Сеанс кончается уходом из ленты (`tab_gone{navigated_away}` уже уходит): запомнить, из какого раздела. */
  sessionLeftFeed(): void;
  /** Откуда сеанс ушёл из ленты (`FeedLeftPlace`). */
  feedLeftPlace(): FeedLeftPlace;
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
  // Адрес вкладки, когда встал текущий ролик (правило «уход до первого шага»).
  let urlAtVideoStart = ports.openedUrl;
  // Когда страница вынула текущий ролик, пока раздел не выучен и адрес ещё тот же (ждём адрес в окне).
  let removedBeforeLearningAtMs: number | undefined;
  // Последнее слово страницы об игре, ушедшее плееру (`undefined` — не было: окно играет, как после передачи).
  let lastPagePlayback: "play" | "pause" | undefined;
  // Откуда сеанс ушёл из ленты (решение 375); до ухода — `not_left`.
  let leftPlace: FeedLeftPlace = NOT_LEFT;
  // Лента одного элемента по списку адресов (16b3c): была ли вкладка в ней и не ушла ли.
  const singleElementFeed = createSingleElementFeedWatch(ports.openedUrl);
  // Тень хоть раз вставала в этом сеансе. До этого новое поколение — начало самого видео сеанса, а не превью.
  let aVideoStood = false;

  const now = (): number => ports.scheduler.now();

  /** Ждёт ли сеанс следующий ролик ленты. */
  function waitingForFeedVideo(): boolean {
    return lostAndWaiting || ports.feedAwaitsNextVideo();
  }

  /** Вкладка была в ленте одного элемента из списка и ушла из неё (решение 381). */
  function leftSingleElementFeed(): boolean {
    return singleElementFeed.state() === "left_feed";
  }

  /** Раздел ленты известен: выучен на шаге ленты или задан списком лент одного элемента. */
  function feedSectionKnown(): boolean {
    return feedSection !== undefined || singleElementFeed.state() !== "never_in_feed";
  }

  /**
   * Вкладка ушла в другой раздел, чем у текущего ролика (неразборный или неизвестный адрес — не ушла). У ленты из
   * списка раздел — сам список: шаг Shorts (`/shorts/<id>`) раздела не меняет, а уход из списка — меняет.
   */
  function sectionChanged(): boolean {
    if (singleElementFeed.state() !== "never_in_feed") return leftSingleElementFeed();
    return feedSection !== undefined && latestSection !== undefined && latestSection !== feedSection;
  }

  /**
   * Пора ли просить конец: вкладка ушла из ленты одного элемента (381) — всегда; иначе — если ждём ролик ленты, а
   * вкладка в другом разделе (F5) или адрес пришёл после потери (F3).
   */
  function leaveIsDue(): boolean {
    return leftSingleElementFeed() || (waitingForFeedVideo() && (sectionChanged() || leaveRequested));
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
      // Лента одного элемента: вход в неё и уход из неё — по списку; отсрочку 381 начнёт `reviewSection` ниже.
      singleElementFeed.addressSeen(url);
      const section = feedSectionOf(url);
      if (section !== latestSection) lastSectionChangeAtMs = now();
      latestSection = section;
      // Адрес самого шага ленты, пришедший чуть позже тени, — это раздел ленты, а не уход из него.
      if (now() <= sectionLearningUntilMs && currentExit === undefined) feedSection = section;
      // Порядок «удаление → адрес» (F3; до выученного раздела — любой новый адрес после вынутого ролика): уход сразу.
      const lossThenSection = unexplainedLossAtMs !== undefined && sectionChanged() && now() - unexplainedLossAtMs <= FEED_EXIT_WINDOW_MS;
      const removalThenAddress = removedBeforeLearningAtMs !== undefined && now() - removedBeforeLearningAtMs <= FEED_EXIT_WINDOW_MS;
      if (lossThenSection || removalThenAddress) {
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
      removedBeforeLearningAtMs = undefined;
      urlAtVideoStart = latestUrl;
      videoStartedAtMs = now();
      // Новое видео: его игру страница назовёт сама (`page_playback` сразу за `handoff_began`), прежнее слово не в счёт.
      lastPagePlayback = undefined;
      aVideoStood = true;
      leaveTimer.cancel();
      // Тень встала на видео, когда вкладка уже ушла из ленты одного элемента (переезд начался до ухода): конец
      // по-прежнему положен, отсрочка — заново.
      if (leftSingleElementFeed()) reviewSection();
    },
    videoLeftScreen(exit) {
      currentExit = exit;
      // Уход до первого шага ленты (шапка): вынутый ролик и адрес, сменившийся с начала ролика. У ленты из списка
      // раздел известен сразу, и адрес Shorts меняется на каждом ролике без переезда — правило молчит (382).
      if (!stopped && exit === "removed_from_document" && !feedSectionKnown()) {
        if (latestUrl !== urlAtVideoStart) {
          leaveRequested = true;
          requestLeave();
          return;
        }
        removedBeforeLearningAtMs = now();
      }
      reviewSection();
    },
    currentVideoLost() {
      if (stopped) return "not_feed";
      // 381: вкладка уже вне ленты одного элемента (отсрочка конца ещё идёт) — это уход, а не потеря ролика.
      if (leftSingleElementFeed()) return "left_feed";
      // 382: адрес в ленте из списка — это лента, даже без единого переезда.
      const knownFeed = feedStepTaken || ports.feedAwaitsNextVideo() || singleElementFeed.state() === "in_feed";
      if (!knownFeed) return "not_feed";
      const scrolledAway = currentExit === "scrolled_off_screen";
      if (!scrolledAway && sectionLeftWithinExitWindow()) return "left_feed";
      if (!scrolledAway) unexplainedLossAtMs = now();
      lostAndWaiting = true;
      reviewSection();
      return "feed_continues";
    },
    heldVideoPlace() {
      // Уход уже доказан (или вкладка вне ленты одного элемента), а конец ждёт отсрочки или конца переезда:
      // задержанное видео — из нового раздела.
      if (leaveRequested || leftSingleElementFeed()) return "other_section";
      if (!waitingForFeedVideo()) return "not_a_feed_move";
      return sectionChanged() ? "other_section" : "feed_section";
    },
    pageGenerationBegan() {
      // До первой тени поколение — начало самого видео сеанса. После неё фаза `beginning` уже не наступает, и ответ
      // `ended` значит, что сеанс действительно кончился (`active`/`between_videos`), а `not_now` — что он переезжает.
      if (stopped || !aVideoStood || !leftSingleElementFeed()) return "forward";
      // Вкладка вне ленты одного элемента, а страница дала новый MediaSource (превью главной на том же элементе):
      // конец сейчас, не дожидаясь отсрочки, — иначе превью стало бы роликом окна.
      leaveTimer.cancel();
      if (ports.leaveFeed() === "ended") return "session_ended";
      // Посреди переезда поколение — у видео, на которое сеанс уже переезжает; конец — после отсрочки.
      leaveTimer.start(FEED_SECTION_LEAVE_GRACE_MS, recheckLeave);
      return "forward";
    },
    pagePlaybackForwarded(request) {
      if (request !== "set_rate") lastPagePlayback = request;
    },
    windowPausedByPage: () => lastPagePlayback === "pause",
    sessionLeftFeed() {
      // Лента из списка — её раздел (Shorts не переезжает, и адрес старта ролика может быть ещё не лентой: стенд).
      // До первого шага раздел не выучен: лента — раздел адреса, на котором встал ушедший ролик.
      const startSection = urlAtVideoStart === undefined ? undefined : feedSectionOf(urlAtVideoStart);
      const section = singleElementFeed.lastFeedSection() ?? feedSection ?? startSection;
      leftPlace = section === undefined ? NOT_LEFT : { kind: "left", section };
    },
    feedLeftPlace: () => leftPlace,
    stop() {
      stopped = true;
      leaveTimer.cancel();
    },
  };
}
