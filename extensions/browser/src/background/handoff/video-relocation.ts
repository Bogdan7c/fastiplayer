// Переезд сеанса на следующее видео (сессия 09c2, решения 201, 204): что знает фон о том, можно ли
// сейчас переехать и сколько ещё ждать.
//
// Владеет:
// - признаком «на экране реклама». Он живёт у сеанса, а не у элемента: ролики цепочки сменяют друг
//   друга, а признак обязан пережить переезд. Сессия 09d читает его только через `adOnScreen`;
// - взводом сторожа следующего видео во всех фреймах вкладки. Взвод — срок, который фон продлевает,
//   пока он нужен: новый фрейм получит его с ближайшим продлением, а умерший фон не оставит страницу
//   со взведённым сторожем;
// - ожиданием следующего ролика и его пределом. Сессия 16a (решения 303, 308): после конца серии
//   (часы плеера сказали `ended`, ролик — не реклама) ждём `NEXT_EPISODE_WAIT_MS` (60 с), в остальных
//   случаях — прежние `NEXT_VIDEO_WAIT_MS` (15 с). Вид ожидания запоминается в миг его начала: часы
//   плеера продолжают приходить и между роликами, и их `ended` могло бы смениться до истечения срока.
//
// - признаком «лента листается» (16b2, решения 354–356, 359): наш ролик ушёл с экрана, и страница сама его
//   остановила. Тогда переезд открыт для ролика, который страница запустит следующим, тихий ролик ленты
//   тоже годится (решение 357), а ожидание идёт без предела — пока человек не закроет окно. Признак держится
//   до следующего видео; страница запустила наш ролик снова — признак снят (вернулись к нему). Превью по
//   наведению переезд не открывает: при нём наш ролик на экране или играет;
//
// - взводом сторожа на время ожидания после перезагрузки страницы (сессия 16c): документ ушёл, срок ведёт
//   session-page-reload.ts, а переезд только держит сторож взведённым и свой срок не начинает (два срока
//   наперегонки кончали бы сеанс по тому, что сработало раньше).
//
// Сторож взведён чаще, чем открыт переезд, и это намеренно. Короткий файл с вердиктом «фильм» может
// оказаться рекламой, которую правила не узнали: сторож должен успеть за `play()` фильма, а переезд
// откроется только когда ролик дошёл до конца. Иначе превью по наведению увело бы сеанс с настоящего видео.
//
// Часы и таймеры — через `Scheduler` фона: таймеры страницы в скрытой вкладке спят, таймеры фона нет.

import type { ClockState } from "../../protocol/session/player.ts";
import type { ClockRelation } from "../../shared/handoff-contract.ts";
import { NEXT_EPISODE_WAIT_MS } from "../../shared/next-episode-wait.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { createTimerSlot } from "../host-link/timer-slot.ts";
import type { VideoRole } from "./video-choice.ts";

/**
 * Сколько ждём следующий ролик. Замер живых сайтов: фильм стартует сразу, между роликами цепочки до
 * 1,3 с. Запас — на запрос рекламной сети (VAST) и на таймеры скрытой вкладки, которые тикают раз в
 * секунду. Дольше окно с последним кадром выглядело бы зависшим. Вкладка, скрытая больше пяти минут,
 * тикает раз в минуту и в этот предел не укладывается: после конца серии ждём дольше (16a, ниже).
 */
export const NEXT_VIDEO_WAIT_MS = 15_000;

/** Срок одного взвода сторожа: три продления подряд могут потеряться. */
export const NEXT_VIDEO_ARM_LEASE_MS = 3_000;

/** Как часто фон продлевает взвод. С этой же частотой сторож обходит видео каждого фрейма. */
export const NEXT_VIDEO_ARM_REFRESH_MS = 1_000;

/**
 * Какое ожидание идёт:
 * - `after_episode_end` — серия (не реклама) доиграла у плеера, страница ставит следующую: срок
 *   `NEXT_EPISODE_WAIT_MS`, по его концу — «The page didn't start the next episode»;
 * - `next_video` — реклама кончилась или видео пропало посреди ролика: срок `NEXT_VIDEO_WAIT_MS`, по его
 *   концу — «страница забрала видео»;
 * - `next_in_feed` — лента листается (16b2): наш ролик ушёл с экрана, страница его остановила. Срока нет
 *   (решение 356): окно стоит на последнем кадре, пока человек его не закроет или вкладка не уйдёт.
 */
export type NextVideoWait = "after_episode_end" | "next_video" | "next_in_feed";

/** Предел ожидания: срок в миллисекундах или «пока человек не закроет окно». */
export type NextVideoWaitLimit = { readonly kind: "timed"; readonly ms: number } | { readonly kind: "until_viewer_closes" };

/** Предел ожидания каждого вида. */
export function nextVideoWaitLimit(wait: NextVideoWait): NextVideoWaitLimit {
  switch (wait) {
    case "after_episode_end":
      return { kind: "timed", ms: NEXT_EPISODE_WAIT_MS };
    case "next_video":
      return { kind: "timed", ms: NEXT_VIDEO_WAIT_MS };
    case "next_in_feed":
      return { kind: "until_viewer_closes" };
  }
}

/**
 * Брать ли тихий ролик ленты (`isSilentClipInPlayerPlace`, решение 357) при переезде. Вне ленты — нет: после
 * рекламы рядом может играть беззвучный баннер, и сеанс не должен на него уйти (страница `ad-then-banner`).
 */
export type SilentClipAdoption = "accepted" | "refused";

/** Тик часов плеера, как его видит переезд: состояние и отношение к текущему видео (16a2). */
export interface RelocationClockTick {
  readonly state: ClockState;
  readonly relation: ClockRelation;
}

/**
 * Как вернуть странице прежнее видео при переезде (16b3b, решение 372):
 * - `at_its_second` — шаг ленты: прежний ролик остаётся на той секунде, где страница его остановила, как у
 *   самого сайта (Instagram продолжает живой элемент со своей секунды, если к нему вернуться);
 * - `at_start` — реклама, ролик «может быть рекламой», видео, доигравшее до конца, и любой другой переезд:
 *   начало и пауза (правило 09c2). У доигравшего конец файла дал бы странице второе `ended`.
 */
export type PreviousVideoLeave = "at_its_second" | "at_start";

/** На каком видео стоит сеанс. */
export interface RelocationVideo {
  readonly role: VideoRole;
  readonly mayBeAd: boolean;
}

export interface VideoRelocation {
  /** Тень встала на новое видео: первое в сеансе или после переезда. */
  videoStarted(video: RelocationVideo): void;
  /**
   * Тик часов плеера. Часы прошлого видео (`previous_video`) переезду ничего не говорят: их `ended` не
   * открывает переезд снова и не начинает ожидание серии. Без номера (`unknown`, сторона 1.9) — запасное
   * правило «после переезда чужой `ended` до первого живого тика не считается».
   */
  noteClockState(tick: RelocationClockTick): void;
  /**
   * Тени нет: страница убрала видео, а следующего ещё нет. Начинается ожидание (или продолжается
   * прежнее, если переезд не удался); ответ — вид идущего ожидания.
   */
  videoLeft(): NextVideoWait;
  /**
   * Текущее видео — серия (не реклама), и часы плеера сказали для него `ended`. Фон верит этому, а не
   * странице: донесение тени «жду следующую серию» без него плееру не передаётся.
   */
  episodeEnded(): boolean;
  /** Можно ли переехать прямо сейчас. Видео, которое ещё идёт и не реклама, переезд не отдаёт. */
  adoptionOpen(): boolean;
  /**
   * Страница убрала видео: это повод ждать следующее или она забрала видео насовсем (решение 179)?
   * Шире `adoptionOpen`: неузнанную рекламу пропускают посреди ролика, и конец до неё не дойдёт.
   */
  nextVideoExpectedAfterLoss(): boolean;
  /** На экране реклама (сессия 09d читает только это). */
  adOnScreen(): boolean;
  /**
   * Тень донесла: наш ролик ушёл с экрана, и страница его остановила (`video_left_screen`, 16b2). Лента
   * листается: сторож взводится, переезд открыт, ожидание — без предела.
   */
  ourVideoLeftScreen(): void;
  /** Страница снова запустила наш ролик (`page_playback{play}`): к нему вернулись, лента не листается. */
  ourVideoResumed(): void;
  /** Годится ли сейчас для переезда тихий ролик ленты. */
  silentClipAdoption(): SilentClipAdoption;
  /**
   * Лента листается и сеанс ждёт её следующий ролик (16b3): наш ролик ушёл с экрана или пропал в сеансе-ленте.
   * Тень при этом может ещё стоять на остановленном ролике (фаза `active`) или уже снята (`between_videos`).
   */
  waitsForNextFeedVideo(): boolean;
  /** Как вернуть странице текущее видео, если сеанс сейчас переезжает с него (`PreviousVideoLeave`). */
  howToLeavePreviousVideo(): PreviousVideoLeave;
  /**
   * Сеанс-лента потерял текущий ролик без ухода прокруткой (16b3, правило F2: страница удалила элемент или сняла
   * источник, адрес не ушёл в другой раздел): лента продолжается — ожидание `next_in_feed`, без предела, как после
   * ухода с экрана. Зовётся ДО `videoLeft`, иначе ожидание начнётся как короткое `next_video`.
   */
  feedVideoLost(): void;
  /**
   * Документ страницы ушёл, сеанс ждёт видео нового документа (16c): сторож взведён во всех фреймах до
   * `stop()`, а свой срок ожидания переезд снимает и больше не начинает — срок у ожидания перезагрузки свой.
   */
  pageReloadBegan(): void;
  /** Сеанс кончился: снять таймеры. Взвод истечёт сам. */
  stop(): void;
}

export interface VideoRelocationDependencies {
  readonly scheduler: Scheduler;
  /** Взвести сторож всем фреймам вкладки на `leaseMs`. Сбой доставки не важен: продление придёт. */
  readonly armFrames: (leaseMs: number) => void;
  /** Предел ожидания вышел, следующего видео нет: честный конец (вид ожидания решает, какой). */
  readonly onWaitExpired: (wait: NextVideoWait) => void;
}

export function createVideoRelocation(dependencies: VideoRelocationDependencies): VideoRelocation {
  const armTimer = createTimerSlot(dependencies.scheduler);
  const waitTimer = createTimerSlot(dependencies.scheduler);
  let role: VideoRole = "film";
  let mayBeAd = false;
  // Текущее видео дошло до конца: страница получила `ended`.
  let endedReached = false;
  // Тени нет, ждём следующее видео.
  let waiting = false;
  // Запасное правило для часов без номера (сторона 1.9): после переезда плеер ещё шлёт часы старого
  // поколения, их `ended` к новому видео не относится. С номером (1.10) это знает пометка тика.
  let staleClockExpected = false;
  // Предел вышел, пока сеанс переезжал: неудавшийся переезд новый срок не получает. Иначе страница,
  // которая подсовывает видео и не отвечает на `begin_handoff`, держала бы окно бесконечно.
  let waitExpired = false;
  // Вид идущего ожидания: запоминается в миг начала и живёт до следующего видео.
  let currentWait: NextVideoWait | undefined;
  // Документ ушёл (16c): сторож нужен новому документу, а срок ведёт ожидание перезагрузки.
  let pageReloading = false;
  // Лента листается (16b2): наш ролик ушёл с экрана, и страница его остановила.
  let feedScrolledAway = false;

  function armsNeeded(): boolean {
    return role === "ad" || mayBeAd || endedReached || waiting || pageReloading || feedScrolledAway;
  }

  /**
   * Реклама кончилась и страница её не убрала, или тени уже нет: идёт отсчёт. Во время ожидания перезагрузки
   * — никогда: часы плеера идут и тогда (окно досматривает скачанное), и их `ended` не должен начать второй срок.
   */
  function waitsForNextVideo(): boolean {
    return !pageReloading && (waiting || (endedReached && role === "ad"));
  }

  /**
   * Доиграла ли у плеера именно серия. Реклама и короткий ролик «может быть рекламой» (`mayBeAd`,
   * не длиннее преролла) серией не считаются: после них страница ставит фильм быстро, а если не
   * ставит — прежний короткий срок честно кончает сеанс (страница `ad-no-film`, решение 204).
   */
  function episodeEndReached(): boolean {
    return endedReached && role !== "ad" && !mayBeAd;
  }

  /** Вид ожидания, которое начинается сейчас: лента — без предела, конец серии — долгое, остальное — короткое. */
  function waitStartingNow(): NextVideoWait {
    if (feedScrolledAway) return "next_in_feed";
    return episodeEndReached() ? "after_episode_end" : "next_video";
  }

  function renewArming(): void {
    dependencies.armFrames(NEXT_VIDEO_ARM_LEASE_MS);
    armTimer.start(NEXT_VIDEO_ARM_REFRESH_MS, renewArming);
  }

  function applyState(): void {
    if (!armsNeeded()) armTimer.cancel();
    else if (!armTimer.isActive()) renewArming();
    if (!waitsForNextVideo()) {
      waitTimer.cancel();
      currentWait = undefined;
      return;
    }
    const wait = currentWait ?? waitStartingNow();
    currentWait = wait;
    if (waitExpired) {
      dependencies.onWaitExpired(wait);
      return;
    }
    const limit = nextVideoWaitLimit(wait);
    // Лента (решение 356): срока нет — честный конец дадут закрытое окно, закрытая вкладка или уход страницы.
    if (limit.kind === "until_viewer_closes") return;
    if (!waitTimer.isActive()) {
      waitTimer.start(limit.ms, () => {
        waitExpired = true;
        dependencies.onWaitExpired(wait);
      });
    }
  }

  return {
    videoStarted(video) {
      staleClockExpected = endedReached;
      role = video.role;
      mayBeAd = video.mayBeAd;
      endedReached = false;
      waiting = false;
      waitExpired = false;
      currentWait = undefined;
      // Тень встала (например, на вернувшейся странице сироты, 11b2): ожидание перезагрузки позади.
      pageReloading = false;
      // Новый ролик на экране: прежнее «ушёл с экрана» к нему не относится.
      feedScrolledAway = false;
      waitTimer.cancel();
      applyState();
    },
    noteClockState(tick) {
      switch (tick.relation) {
        case "previous_video":
          // Часы прошлого видео: ни конец, ни игра текущего видео из них не следуют.
          return;
        case "this_video":
          // Номер говорит точно: это часы текущего видео, запасное правило больше не нужно.
          staleClockExpected = false;
          break;
        case "unknown":
          if (staleClockExpected) {
            if (tick.state === "ended") return;
            staleClockExpected = false;
          }
          break;
      }
      endedReached = tick.state === "ended";
      applyState();
    },
    videoLeft() {
      waiting = true;
      applyState();
      // `applyState` при `waiting` всегда запоминает вид ожидания; запасной ответ — для полноты типа.
      return currentWait ?? waitStartingNow();
    },
    episodeEnded: episodeEndReached,
    adoptionOpen: () => role === "ad" || endedReached || waiting || feedScrolledAway,
    nextVideoExpectedAfterLoss: () => role === "ad" || mayBeAd || endedReached || waiting || feedScrolledAway,
    adOnScreen: () => role === "ad",
    ourVideoLeftScreen() {
      feedScrolledAway = true;
      applyState();
    },
    ourVideoResumed() {
      // Между роликами тени нет, и «наш» ролик уже не наш: снимать признак нечем и незачем.
      if (waiting) return;
      feedScrolledAway = false;
      applyState();
    },
    silentClipAdoption: () => (feedScrolledAway ? "accepted" : "refused"),
    waitsForNextFeedVideo: () => feedScrolledAway,
    howToLeavePreviousVideo() {
      // Шаг ленты — только ролик, который ушёл прокруткой посреди просмотра и не похож на рекламу.
      const feedStepMidReel = feedScrolledAway && role !== "ad" && !mayBeAd && !endedReached;
      return feedStepMidReel ? "at_its_second" : "at_start";
    },
    feedVideoLost() {
      feedScrolledAway = true;
      applyState();
    },
    pageReloadBegan() {
      pageReloading = true;
      applyState();
    },
    stop() {
      pageReloading = false;
      armTimer.cancel();
      waitTimer.cancel();
    },
  };
}
