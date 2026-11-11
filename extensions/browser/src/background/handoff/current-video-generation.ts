// Поколение текущего видео сеанса и отношение тика часов к нему (протокол 1.10, сессия 16a2, решение 311).
//
// Зачем. Плеер переходит с одного поколения потока на другое не мгновенно: пока данных нового ролика нет, он
// показывает прошлый и шлёт ЕГО часы. С 1.10 тик называет поколение, которое описывает (`clock.generation`),
// но «прошлое» ли это поколение, знает только фон: он раздаёт номера (`stream-numbering.ts`) и видит миг, когда
// тень встала на новое видео (`handoff_began{started}`).
//
// Владеет одним числом — «последнее поколение, объявленное ДО текущего видео» (нижняя граница). Объявленные
// поколения не копирует: последний номер всегда спрашивается у нумерации (MSE, файл и HLS — общий счёт).
//
// Правило пометки тика с номером `g` (`latest` — последнее объявленное поколение, `floor` — граница):
// 1. Номера нет — `unknown` (сторона 1.9 или у плеера ещё нет поколения).
// 2. Общая версия ниже 1.10 — `unknown`: номеру такой стороны верить нельзя (CLOCK_GENERATION_INTRODUCED_IN).
// 3. `g > latest` (или поколений ещё нет) — `unknown`. Так быть не должно (плеер не знает номеров, которых фон
//    не объявлял); прежние правила безопаснее, чем «прошлое видео»: прошлое никогда не подтверждает переезд тени,
//    и ошибка в номере навсегда заморозила бы её.
// 4. `g === latest` и `latest > floor` (или границы нет — первое видео сеанса) — `this_video`.
// 5. Остальное — `previous_video`: номер меньше последнего (прошлый MediaSource того же элемента, прошлый
//    ролик) или окно переезда — тень уже на новом видео, а его поколение ещё не объявлено (`latest === floor`),
//    значит, тик последнего номера описывает прошлое видео.
//
// Инвариант, на котором стоит граница: поколение нового видео объявляется ПОСЛЕ его `handoff_began`. Тень
// `previous_video` не отпускает никогда (`shadow/position-hold.ts`), так что нарушение заморозило бы её. Держится так:
// - страница шлёт `handoff_began{started}` раньше повтора журнала и события источника (`handoff-controller.ts`
//   `#beginHandoff`), а порт доставляет по порядку;
// - журнал MediaSource доходит до фона, только пока он подключён к переданному элементу (`liveMediaSources`):
//   новый MediaSource того же элемента — та же тень, без переезда; чужого элемента — не объявляется вовсе;
// - файл и HLS открывают новое поколение всегда (`startFileGeneration`), новый документ — свои ключи MSE;
// - повторное подключение уже объявленного MediaSource и так кончает сеанс (`source_buffer_announced_twice`).
// Известное окно: новый MediaSource на том же элементе без переезда — до его объявления тики прошлого
// поколения ещё `this_video` (последний номер), тень судит их по прежним правилам.
//
// Тики по этой пометке НЕ выбрасываются: на них бьётся пульс тени (фон кончает передачу после 10 с тишины,
// `shadow-silence-watch.ts`), а при смене качества прошлое поколение — то же место того же фильма.

import { CLOCK_GENERATION_INTRODUCED_IN, versionIncludes, type ProtocolVersion, type StreamGeneration } from "../../protocol/index.ts";
import type { ClockRelation } from "../../shared/handoff-contract.ts";
import type { StreamNumbering } from "./stream-numbering.ts";

/** Номера, по которым решается пометка тика. */
export interface GenerationBounds {
  /** Последнее объявленное поколение сеанса (`undefined` — ещё ни одного). */
  readonly latest: StreamGeneration | undefined;
  /** Последнее поколение, объявленное до старта текущего видео (`undefined` — первое видео сеанса). */
  readonly floor: StreamGeneration | undefined;
}

/**
 * Чистое правило пометки (шапка модуля, пункты 1, 3–5). Версию протокола сверяет владелец: здесь только номера.
 */
export function classifyClockGeneration(tickGeneration: StreamGeneration | undefined, bounds: GenerationBounds): ClockRelation {
  const { latest, floor } = bounds;
  if (tickGeneration === undefined) return "unknown";
  // Номер, которого фон не объявлял: не «прошлое видео», а неизвестность (пункт 3).
  if (latest === undefined || tickGeneration > latest) return "unknown";
  const latestBelongsToCurrentVideo = floor === undefined || latest > floor;
  if (tickGeneration === latest && latestBelongsToCurrentVideo) return "this_video";
  return "previous_video";
}

/** Владелец «поколения текущего видео» для сеанса передачи. */
export interface CurrentVideoGeneration {
  /** Последнее объявленное поколение сеанса (`undefined` — ещё ни одного). Правда — у нумерации. */
  latestGeneration(): StreamGeneration | undefined;
  /**
   * Тень встала на новое видео (`handoff_began{started}`: первое в сеансе или после переезда). Всё, что
   * объявлено до этого мига, для нового видео — прошлое.
   */
  videoStarted(): void;
  /** К какому видео относится тик часов с этим номером поколения (шапка модуля). */
  clockRelationOf(tickGeneration: StreamGeneration | undefined): ClockRelation;
}

export interface CurrentVideoGenerationDependencies {
  /** Нумерация сеанса: единственный источник объявленных номеров. */
  readonly numbering: Pick<StreamNumbering, "latestGeneration">;
  /** Общая версия протокола (до `granted` — 1.0). */
  readonly agreedProtocol: () => ProtocolVersion;
}

export function createCurrentVideoGeneration(dependencies: CurrentVideoGenerationDependencies): CurrentVideoGeneration {
  let floor: StreamGeneration | undefined;
  return {
    latestGeneration: () => dependencies.numbering.latestGeneration(),
    videoStarted() {
      floor = dependencies.numbering.latestGeneration();
    },
    clockRelationOf(tickGeneration) {
      // Пункт 2: сторона ниже 1.10 номера не шлёт; если всё же прислала — ему не верим, правила прежние.
      if (!versionIncludes(dependencies.agreedProtocol(), CLOCK_GENERATION_INTRODUCED_IN)) return "unknown";
      return classifyClockGeneration(tickGeneration, { latest: dependencies.numbering.latestGeneration(), floor });
    },
  };
}
