// Удержание позиции тени (`PositionHold`): тень стоит на цели, пока часы плеера не подтвердят прыжок.
// Чистые правила без DOM и без модели тени: какой тик подтверждает удержание, а какой только учитывается.
//
// Вынесено из `shadow-timeline.ts` в сессии 16a2: правила подтверждения стали зависеть от того, к какому видео
// относится тик (`ClockRelation`, протокол 1.10), и это один связный инвариант — «чужие часы не двигают и не
// кончают наше видео». Модель тени решает, ЧТО делать по итогу; этот модуль — ПОДТВЕРЖДЁН ли прыжок.

import type { ClockRelation } from "../../shared/handoff-contract.ts";
import type { ClockState } from "../../protocol/session/player.ts";
import type { MediaTimeMicros } from "../../protocol/time.ts";

/**
 * Перемотку страницы подтверждает тик, близкий к цели: тики, отправленные плеером до того,
 * как он узнал о `page_seek`, ещё несут старую позицию и не должны «отматывать» страницу назад.
 */
export const PAGE_SEEK_CONFIRM_TOLERANCE_US = 1_000_000;

/**
 * Перемотка страницы: если плеер встал не туда (живой эфир, обрезка окна DVR), восемь тиков
 * (~2 с) без подтверждения — и следующий тик принимается как есть: перемотка не висит вечно.
 * Смену источника это правило НЕ касается (там чужие тики — часы старого поколения, Д-4).
 */
export const MAX_UNCONFIRMED_TICKS = 8;

/**
 * Страховочный выход для смены источника (Д-4), когда плеер так и не признал новый источник
 * (ни тика у цели, ни тика ожидания): через 15 с тень принимает позицию часов, но НИКОГДА — их
 * `ended`. Меньше нельзя: плеер ещё показывает старое поколение, пока данные нового (рекламы)
 * не пришли, и его часы несут позицию старого фильма; 15 с покрывают медленную загрузку первого
 * куска рекламы. Выход нужен на случай плеера, который не шлёт «0 / waiting» (иначе тень стояла
 * бы на 0 вечно). Часы, помеченные `previous_video`, этим выходом не пользуются: их номер точно
 * говорит, что новое видео ещё не в плеере.
 */
export const SOURCE_RESET_GIVE_UP_MS = 15_000;

/** Кто двигает позицию: плеер, страница или сама смена источника. */
export type PositionHoldKind = "player_seek" | "page_seek" | "source_reset";

/** Позиция «заморожена» на цели, пока часы плеера не подтвердят прыжок. */
export interface PositionHold {
  readonly kind: PositionHoldKind;
  readonly targetUs: MediaTimeMicros;
  /** Цель — конец ролика: после подтверждения тень переходит в `ended`. */
  readonly landsAtEnd: boolean;
  /** Перемотки: сколько ещё чужих тиков стерпеть до принятия позиции часов. */
  unconfirmedTicksLeft: number;
  /** Когда hold поставлен (страховочный выход смены источника считается от него). */
  readonly startedAtMs: number;
  /**
   * Смена источника: плеер уже прислал тик ожидания («0 / waiting», пока новое поколение не
   * показано) — значит, он признал новый источник, и следующий тик несёт уже его позицию.
   * Тик прошлого видео (`previous_video`) этот признак не ставит: его «жду» — про чужой ролик.
   */
  playerAcknowledgedNewSource: boolean;
}

/** Часть тика, по которой решается судьба удержания. */
export interface HoldTick {
  readonly positionUs: MediaTimeMicros;
  readonly state: ClockState;
  readonly relation: ClockRelation;
}

/** Что задаёт новое удержание; остальное (терпение, признание) начинается с нуля. */
export type PositionHoldTarget = Pick<PositionHold, "kind" | "targetUs" | "landsAtEnd">;

/** Новое удержание с полным запасом терпения. */
export function newPositionHold(target: PositionHoldTarget, nowMs: number): PositionHold {
  return { ...target, unconfirmedTicksLeft: MAX_UNCONFIRMED_TICKS, startedAtMs: nowMs, playerAcknowledgedNewSource: false };
}

/** Подтверждает ли тик прыжок, которого ждёт тень. */
export function holdConfirmedBy(hold: PositionHold, tick: HoldTick, nowMs: number): boolean {
  switch (hold.kind) {
    case "player_seek":
      // Команды плеера и его часы идут по одному каналу по порядку: тик после `seek` — уже новый.
      return true;
    case "page_seek":
      return withinTolerance(hold, tick) || hold.unconfirmedTicksLeft <= 0;
    case "source_reset":
      return sourceResetConfirmedBy(hold, tick, nowMs);
  }
}

/** Учесть неподтвердивший тик: счётчик перемотки и признак «плеер признал новый источник». */
export function noteUnconfirmedTick(hold: PositionHold, tick: Pick<HoldTick, "state" | "relation">): void {
  hold.unconfirmedTicksLeft -= 1;
  if (tick.state === "waiting" && tick.relation !== "previous_video") hold.playerAcknowledgedNewSource = true;
}

function withinTolerance(hold: PositionHold, tick: HoldTick): boolean {
  return Math.abs(tick.positionUs - hold.targetUs) <= PAGE_SEEK_CONFIRM_TOLERANCE_US;
}

/**
 * Смена источника: тень ждёт, пока плеер покажет НОВОЕ видео. Правила по пометке тика:
 * - `previous_video` — никогда: ни у цели (баннер кончился на 12,0 с, фильм стартует с 11,1 с — допуск ±1 с
 *   принял бы конец баннера за старт фильма), ни по страховке времени, ни после признания;
 * - `this_video` — прежние правила: у цели, после признания или по страховке; чужой `ended` вне цели — нет.
 *   Тик нового поколения не снимает удержание сразу: его «0 / жду» может проскочить раньше `page_seek{T0}`;
 * - `unknown` (сторона 1.9) — прежние правила плюс запасное: `ended` не подтверждает удержание никогда, даже
 *   у цели. Это конец СТАРОГО поколения (решение 146): у тени, которая только что встала на элемент, своего
 *   конца без единого тика игры не бывает.
 */
function sourceResetConfirmedBy(hold: PositionHold, tick: HoldTick, nowMs: number): boolean {
  switch (tick.relation) {
    case "previous_video":
      return false;
    case "unknown":
      if (tick.state === "ended") return false;
      return withinTolerance(hold, tick) || hold.playerAcknowledgedNewSource || giveUpReached(hold, nowMs);
    case "this_video":
      if (withinTolerance(hold, tick) || hold.playerAcknowledgedNewSource) return true;
      return tick.state !== "ended" && giveUpReached(hold, nowMs);
  }
}

function giveUpReached(hold: PositionHold, nowMs: number): boolean {
  return nowMs - hold.startedAtMs >= SOURCE_RESET_GIVE_UP_MS;
}
