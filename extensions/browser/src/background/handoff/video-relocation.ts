// Переезд сеанса на следующее видео (сессия 09c2, решения 201, 204): что знает фон о том, можно ли
// сейчас переехать и сколько ещё ждать.
//
// Владеет:
// - признаком «на экране реклама». Он живёт у сеанса, а не у элемента: ролики цепочки сменяют друг
//   друга, а признак обязан пережить переезд. Сессия 09d читает его только через `adOnScreen`;
// - взводом сторожа следующего видео во всех фреймах вкладки. Взвод — срок, который фон продлевает,
//   пока он нужен: новый фрейм получит его с ближайшим продлением, а умерший фон не оставит страницу
//   со взведённым сторожем;
// - ожиданием следующего ролика и его пределом.
//
// Сторож взведён чаще, чем открыт переезд, и это намеренно. Короткий файл с вердиктом «фильм» может
// оказаться рекламой, которую правила не узнали: сторож должен успеть за `play()` фильма, а переезд
// откроется только когда ролик дошёл до конца. Иначе превью по наведению увело бы сеанс с настоящего видео.
//
// Часы и таймеры — через `Scheduler` фона: таймеры страницы в скрытой вкладке спят, таймеры фона нет.

import type { ClockState } from "../../protocol/session/player.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { createTimerSlot } from "../host-link/timer-slot.ts";
import type { VideoRole } from "./video-choice.ts";

/**
 * Сколько ждём следующий ролик. Замер живых сайтов: фильм стартует сразу, между роликами цепочки до
 * 1,3 с. Запас — на запрос рекламной сети (VAST) и на таймеры скрытой вкладки, которые тикают раз в
 * секунду. Дольше окно с последним кадром выглядело бы зависшим. Вкладка, скрытая больше пяти минут,
 * тикает раз в минуту и в этот предел не укладывается: это случай сессии 16.
 */
export const NEXT_VIDEO_WAIT_MS = 15_000;

/** Срок одного взвода сторожа: три продления подряд могут потеряться. */
export const NEXT_VIDEO_ARM_LEASE_MS = 3_000;

/** Как часто фон продлевает взвод. С этой же частотой сторож обходит видео каждого фрейма. */
export const NEXT_VIDEO_ARM_REFRESH_MS = 1_000;

/** На каком видео стоит сеанс. */
export interface RelocationVideo {
  readonly role: VideoRole;
  readonly mayBeAd: boolean;
}

export interface VideoRelocation {
  /** Тень встала на новое видео: первое в сеансе или после переезда. */
  videoStarted(video: RelocationVideo): void;
  /** Тик часов плеера для текущего видео. */
  noteClockState(state: ClockState): void;
  /** Тени нет: страница убрала видео, а следующего ещё нет. Начинается ожидание. */
  videoLeft(): void;
  /** Можно ли переехать прямо сейчас. Видео, которое ещё идёт и не реклама, переезд не отдаёт. */
  adoptionOpen(): boolean;
  /**
   * Страница убрала видео: это повод ждать следующее или она забрала видео насовсем (решение 179)?
   * Шире `adoptionOpen`: неузнанную рекламу пропускают посреди ролика, и конец до неё не дойдёт.
   */
  nextVideoExpectedAfterLoss(): boolean;
  /** На экране реклама (сессия 09d читает только это). */
  adOnScreen(): boolean;
  /** Сеанс кончился: снять таймеры. Взвод истечёт сам. */
  stop(): void;
}

export interface VideoRelocationDependencies {
  readonly scheduler: Scheduler;
  /** Взвести сторож всем фреймам вкладки на `leaseMs`. Сбой доставки не важен: продление придёт. */
  readonly armFrames: (leaseMs: number) => void;
  /** Предел ожидания вышел, следующего видео нет: честный конец. */
  readonly onWaitExpired: () => void;
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
  // После переезда плеер ещё шлёт часы старого поколения: их `ended` к новому видео не относится.
  let staleClockExpected = false;
  // Предел вышел, пока сеанс переезжал: неудавшийся переезд новый срок не получает. Иначе страница,
  // которая подсовывает видео и не отвечает на `begin_handoff`, держала бы окно бесконечно.
  let waitExpired = false;

  function armsNeeded(): boolean {
    return role === "ad" || mayBeAd || endedReached || waiting;
  }

  /** Реклама кончилась и страница её не убрала, или тени уже нет: идёт отсчёт. */
  function waitsForNextVideo(): boolean {
    return waiting || (endedReached && role === "ad");
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
      return;
    }
    if (waitExpired) {
      dependencies.onWaitExpired();
      return;
    }
    if (!waitTimer.isActive()) {
      waitTimer.start(NEXT_VIDEO_WAIT_MS, () => {
        waitExpired = true;
        dependencies.onWaitExpired();
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
      waitTimer.cancel();
      applyState();
    },
    noteClockState(state) {
      if (staleClockExpected) {
        if (state === "ended") return;
        staleClockExpected = false;
      }
      endedReached = state === "ended";
      applyState();
    },
    videoLeft() {
      waiting = true;
      applyState();
    },
    adoptionOpen: () => role === "ad" || endedReached || waiting,
    nextVideoExpectedAfterLoss: () => role === "ad" || mayBeAd || endedReached || waiting,
    adOnScreen: () => role === "ad",
    stop() {
      armTimer.cancel();
      waitTimer.cancel();
    },
  };
}
