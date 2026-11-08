// Сигнал «над этим видео висит предложение «Ask»» (сессия 12b), сторона MAIN-мира.
//
// Тот же приём, что у якоря переданного видео (handed-off-video-anchor.ts, договор —
// src/shared/handed-off-video-contract.ts): событие тайного имени, разосланное ОРИГИНАЛЬНЫМ `dispatchEvent` на
// элементе (предложение встало) или на своём `window` (предложение ушло). Имя — своё: производное от тайного
// имени якоря (`viewerOfferEventNameOf`), поэтому странице оно так же неизвестно.
//
// Канал отдельный от заглушки намеренно: на странице видео A может играть в плеере (заглушка над A), а над
// видео B той же страницы висеть предложение. Конец передачи A не должен снимать панель B, и наоборот.
//
// Якорь ничего не решает: когда предложение встало и ушло, говорит сторож следующего видео.

import { viewerOfferEventNameOf } from "../shared/handed-off-video-contract.ts";
import type { AnchorEventOriginals } from "./handed-off-video-anchor.ts";
import type { OddityReporter } from "./prototype-wrap.ts";

/** То, что сторож зовёт у сигнала предложения (шов: тесты сторожа подставляют фейк). */
export interface ViewerOfferSignals {
  /**
   * Предложение встало над элементом. `false` — изолированный мир о нём не узнал (имени ещё нет, элемент вне
   * документа, сбой рассылки): рисовать панель некому, и сторож не должен держать видео без панели.
   */
  offerStood(element: object): boolean;
  /** Предложение ушло (выбор сделан, снято, видео ушло само). Без названного предложения — ничего. */
  offerEnded(): void;
}

export interface ViewerOfferAnchorEnvironment {
  readonly events: AnchorEventOriginals;
  readonly ownWindow: object;
  readonly isConnected: (element: object) => boolean;
  readonly reportOddity: OddityReporter;
}

export class ViewerOfferAnchor implements ViewerOfferSignals {
  readonly #environment: ViewerOfferAnchorEnvironment;
  #eventName: string | null = null;
  /** Изолированный мир знает о предложении (сигнал «встало» ушёл, «ушло» — ещё нет). */
  #announced = false;

  constructor(environment: ViewerOfferAnchorEnvironment) {
    this.#environment = environment;
  }

  /** Имя события якоря от изолированного мира (то же, что получает якорь заглушки); принимается только первое. */
  setEventName(anchorEventName: string): void {
    if (this.#eventName !== null) return;
    this.#eventName = viewerOfferEventNameOf(anchorEventName);
  }

  offerStood(element: object): boolean {
    const eventName = this.#eventName;
    if (eventName === null || !this.#isConnected(element)) return false;
    // Прежнее предложение без «ушло» (сторож так не делает): сначала честно снять его панель.
    if (this.#announced) this.offerEnded();
    this.#announced = this.#dispatch(element, eventName);
    return this.#announced;
  }

  offerEnded(): void {
    if (!this.#announced || this.#eventName === null) return;
    this.#announced = false;
    this.#dispatch(this.#environment.ownWindow, this.#eventName);
  }

  #isConnected(element: object): boolean {
    try {
      return this.#environment.isConnected(element);
    } catch {
      return false;
    }
  }

  #dispatch(target: object, eventName: string): boolean {
    try {
      this.#environment.events.dispatchComposed(target, eventName);
      return true;
    } catch {
      this.#environment.reportOddity("viewer offer signal could not be dispatched");
      return false;
    }
  }
}
