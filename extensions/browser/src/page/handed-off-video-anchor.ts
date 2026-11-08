// Якорь переданного видео, сторона MAIN-мира (сессия 11a, решение 236; договор —
// src/shared/handed-off-video-contract.ts).
//
// Владеет одним знанием: «на каком элементе сейчас стоит передача, о которой знает изолированный мир».
// Жизнь якоря совпадает с активной передачей контроллера (`handoff-controller.ts`): контроллер зовёт
// `handoffStood` после того, как тень встала, и `handoffEnded` при любом конце передачи. Сам якорь
// ничего не решает о передаче и её не меняет: только рассылает сигналы.
//
// Сигналы — события договорного имени, разосланные ОРИГИНАЛЬНЫМИ `Event` и `dispatchEvent`, снятыми на
// document_start: страница позже может подменить оба, но наш сигнал мимо её подмен не пройдёт.
// Элемент, убранный из документа в момент старта, сигнал не получит (путь события не дойдёт до `window`):
// якорь ждёт, пока элемент вернётся, и пробует снова на тиках часов плеера (не таймером — скрытая
// вкладка их душит, а часы плеера идут и на паузе).

import type { OddityReporter } from "./prototype-wrap.ts";

/** Окно, с которого снимаются оригиналы событий (в тестах — фейк). */
export interface AnchorWindow {
  readonly EventTarget: { readonly prototype: object };
  readonly Event: { new (type: string, init?: EventInit): object };
}

/** Оригиналы событий, нужные якорю. */
export interface AnchorEventOriginals {
  /** Разослать на цели новое событие с этим именем; `composed` — чтобы оно вышло из теневых деревьев. */
  dispatchComposed(target: object, eventName: string): void;
}

/** Снять оригиналы на document_start. Бросает, если окружения нет (тогда якоря нет вовсе). */
export function captureAnchorEventOriginals(win: AnchorWindow): AnchorEventOriginals {
  const dispatchEvent = findMethod(win.EventTarget.prototype, "dispatchEvent");
  const EventConstructor = win.Event;
  return {
    dispatchComposed: (target, eventName) => {
      const event = Reflect.construct(EventConstructor, [eventName, { composed: true }]) as object;
      Reflect.apply(dispatchEvent, target, [event]);
    },
  };
}

function findMethod(prototype: object, name: string): Function {
  let current: object | null = prototype;
  while (current !== null) {
    const value: unknown = Object.getOwnPropertyDescriptor(current, name)?.value;
    if (typeof value === "function") return value;
    current = Object.getPrototypeOf(current) as object | null;
  }
  throw new TypeError(`handed-off video anchor: ${name} is missing`);
}

/** Что якорю нужно от остального MAIN-мира. */
export interface AnchorEnvironment {
  readonly events: AnchorEventOriginals;
  /** Своё окно: сигнал «передача ушла» рассылается на нём. */
  readonly ownWindow: object;
  /** Элемент в документе (оригинальный `isConnected`). */
  readonly isConnected: (element: object) => boolean;
  readonly reportOddity: OddityReporter;
}

/** Передача, о которой якорь сообщает. `announced` — сигнал «передано» уже ушёл. */
interface AnchoredHandoff {
  readonly element: object;
  announced: boolean;
}

/** То, что контроллер передачи зовёт у якоря (шов: тесты контроллера подставляют фейк). */
export interface HandoffAnchorHooks {
  /** Тень на элементе встала: передача активна. */
  handoffStood(element: object): void;
  /** Передача кончилась (любая причина). */
  handoffEnded(): void;
  /** Тик часов плеера: повторить сигнал, если элемент был вне документа. */
  clockTick(): void;
}

export class HandedOffVideoAnchor implements HandoffAnchorHooks {
  readonly #environment: AnchorEnvironment;
  /** Имя события от изолированного мира; `null` — ещё не пришло (или изолированного мира нет). */
  #eventName: string | null = null;
  #anchored: AnchoredHandoff | null = null;

  constructor(environment: AnchorEnvironment) {
    this.#environment = environment;
  }

  /**
   * Имя события от изолированного мира. Принимается только ПЕРВОЕ: порт личный, но второе имя значило бы,
   * что кто-то кроме нашего изолированного мира пишет в порт, — заглушку ему не отдаём.
   */
  setEventName(eventName: string): void {
    if (this.#eventName !== null) {
      this.#environment.reportOddity("second handed-off video event name ignored");
      return;
    }
    this.#eventName = eventName;
    // Передача могла начаться раньше, чем имя дошло: сообщаем о ней сейчас.
    this.#tryAnnounce();
  }

  handoffStood(element: object): void {
    // Новая передача без конца старой (контроллер так не делает): сначала честно «ушло».
    if (this.#anchored !== null && this.#anchored.element !== element) this.handoffEnded();
    if (this.#anchored === null) this.#anchored = { element, announced: false };
    this.#tryAnnounce();
  }

  handoffEnded(): void {
    const anchored = this.#anchored;
    this.#anchored = null;
    // О неназванной передаче изолированный мир не знает: снимать ему нечего.
    if (anchored === null || !anchored.announced || this.#eventName === null) return;
    this.#dispatch(this.#environment.ownWindow, this.#eventName);
  }

  clockTick(): void {
    this.#tryAnnounce();
  }

  /** Есть ли передача, о которой изолированный мир уже знает (для тестов и диагностики). */
  get isAnnounced(): boolean {
    return this.#anchored?.announced === true;
  }

  #tryAnnounce(): void {
    const anchored = this.#anchored;
    const eventName = this.#eventName;
    if (anchored === null || anchored.announced || eventName === null) return;
    if (!this.#isConnected(anchored.element)) return; // путь события не дошёл бы до `window`: ждём тика
    anchored.announced = this.#dispatch(anchored.element, eventName);
  }

  #isConnected(element: object): boolean {
    try {
      return this.#environment.isConnected(element);
    } catch {
      return false;
    }
  }

  /** Разослать сигнал; `false` — не вышло (след в репортёр, странице исключение не уходит). */
  #dispatch(target: object, eventName: string): boolean {
    try {
      this.#environment.events.dispatchComposed(target, eventName);
      return true;
    } catch {
      this.#environment.reportOddity("handed-off video signal could not be dispatched");
      return false;
    }
  }
}
