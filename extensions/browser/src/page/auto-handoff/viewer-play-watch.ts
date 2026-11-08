// Охрана режима 2 «автозапуск» (сессия 12a, решения 246–254): «видео запустил САМ человек, и со звуком»
// (звук и «не цикл» — фильтры видео; охрана знает только их общие включатели, решения 260–261).
//
// Владеет политикой "кто считается зрителем" и больше ничем: включена ли охрана, кредит клика, метки
// `play()` и метки «передача этого элемента кончилась». Правил сайтов, стоп-листа и порога длины здесь нет
// и быть не должно: страница делит мир с MAIN-скриптом, и всё, что здесь лежит, она может прочитать. Сюда
// попадает только то, что страница и так знает (свои клики, свой `play()`); решает фон.
//
// Правило строгое (решения владельца 246, 262): запуск зрителя — это либо клик мышью по ЭТОМУ видео не раньше
// чем за секунду до запуска, либо действующий «кредит клика» от фона (унаследованный клик через короткую рекламу). Защита от зацикливания:
// пока передача элемента не кончилась, ничего не меняется, а после конца (Return, окно закрыто, замена)
// новый запуск того же элемента считается только при НОВОМ доверенном вводе позже конца. Сам клик «Return»
// стоит РАНЬШЕ конца передачи (его ответ идёт кругом через фон), поэтому не засчитывается.
//
// Сторож (`next-video-guard.ts`) зовёт три «судьи» в три момента, и каждый возвращает ответ из ОДНОГО набора:
// - `judgePlayCall` — страница зовёт `play()`; при успехе запоминается метка (видео ещё может ждать метаданных);
// - `judgeTrustedPlayEvent` — доверенное событие `play` (родные кнопки, атрибут `autoplay`);
// - `judgeLoadedMetadata` — метаданные пришли; годится, только если недавно был засчитанный `play()`.
// `seeked` зрителя ничего не начинает вовсе: этого судьи нет.

import { AUTO_HANDOFF_WATCH_OFF, type AutoHandoffWatch } from "../../shared/auto-handoff-contract.ts";
import { MAX_PLAY_CREDIT_LEASE_MS } from "../../shared/auto-handoff-contract.ts";
import type { ViewportRect } from "../dom-readers.ts";
import type { ViewerActivationReader } from "./viewer-activation.ts";

/**
 * Сколько живёт метка засчитанного `play()`, мс. Между `play()` и метаданными ролика проходят сеть и
 * сервер сайта: 15 с покрывает медленный старт, но не позволяет «вчерашнему» клику запустить передачу.
 */
export const PLAY_CALL_MARK_WINDOW_MS = 15_000;

/**
 * Сколько после клика по видео его запуск ещё считается запуском человека, мс (решение 262: «клик по видео +
 * старт сразу»). Сайт зовёт `play()` из обработчика клика или через доли секунды (перерисовка плеера);
 * секунда покрывает это, но не покрывает «кликнул паузу, а сайт через 3 с сам запустил снова».
 */
export const VIEWER_CLICK_WINDOW_MS = 1_000;

/** Слышно ли звук у настоящего элемента (читается оригинальными геттерами: `muted`, `volume`). */
export type ElementAudibility = "audible" | "silent";

/** Ответ судьи: `viewer_started` — запустил зритель; остальное — почему нет (для тестов и диагностики). */
export type ViewerPlayVerdict =
  | "viewer_started"
  | "watch_off"
  | "muted"
  | "handed_back_without_new_input"
  | "no_click_on_video"
  | "no_credited_play_call";

/** Простые факты момента запуска: на них судья не обращается ни к часам, ни к браузеру. */
export interface ViewerPlayFacts {
  /** Охрана: включена ли и пропускает ли сама видео без звука. */
  readonly watch: AutoHandoffWatch;
  /** Человек кликнул по этому видео только что (`VIEWER_CLICK_WINDOW_MS`, решение 262). */
  readonly clickedThisVideo: boolean;
  /** Кредит клика от фона действует. */
  readonly creditActive: boolean;
  /** Когда кончилась передача этого элемента; `null` — не передавался. */
  readonly handedBackAtMs: number | null;
  /** Момент последнего доверенного ввода; `-Infinity` — не было. */
  readonly lastTrustedInputAtMs: number;
  readonly audibility: ElementAudibility;
}

/**
 * Чистое решение «запустил зритель?». Порядок важен: выключенная охрана и заглушенное видео отсекаются
 * первыми; кредит НЕ отменяет метку возврата (возвращённому видео нужен свежий ввод после метки).
 */
export function judgeViewerPlay(facts: ViewerPlayFacts): ViewerPlayVerdict {
  if (facts.watch.state === "off") return "watch_off";
  // Общий фильтр звука выключен (решения 260–261) — беззвучное видео судится дальше: жест всё равно нужен,
  // а включён ли фильтр на этом сайте по списку, решит фон.
  if (facts.audibility === "silent" && facts.watch.muted === "skip") return "muted";
  if (facts.handedBackAtMs !== null && facts.lastTrustedInputAtMs <= facts.handedBackAtMs) return "handed_back_without_new_input";
  if (!facts.clickedThisVideo && !facts.creditActive) return "no_click_on_video";
  return "viewer_started";
}

export interface ViewerPlayWatchEnvironment {
  readonly activation: ViewerActivationReader;
  /** Прямоугольник элемента на экране (оригинальный `getBoundingClientRect`); может бросить. */
  readonly rectOf: (element: object) => ViewportRect;
  /** Монотонные часы окна, мс. */
  readonly now: () => number;
}

export class ViewerPlayWatch {
  readonly #environment: ViewerPlayWatchEnvironment;
  /** Время `play()`, который зритель имел право начать (по нему принимаем `loadedmetadata`). */
  readonly #creditedPlayCalls = new WeakMap<object, number>();
  /** Время конца последней передачи элемента. */
  readonly #handedBack = new WeakMap<object, number>();
  #watch: AutoHandoffWatch = AUTO_HANDOFF_WATCH_OFF;
  #creditUntilMs = Number.NEGATIVE_INFINITY;

  constructor(environment: ViewerPlayWatchEnvironment) {
    this.#environment = environment;
  }

  /** Изолированный мир сообщил: охрана включена или выключена. Выключена по умолчанию (решение 9). */
  setWatch(watch: AutoHandoffWatch): void {
    this.#watch = watch;
  }

  isWatchOn(): boolean {
    return this.#watch.state === "on";
  }

  /** Зацикленное видео охрана пропускает мимо сама (общий фильтр цикла включён). */
  skipsLooped(): boolean {
    return this.#watch.looped === "skip";
  }

  /** Фон выдал «кредит клика» на срок; новый кредит заменяет старый (цепочка реклам продлевает его). */
  grantCredit(leaseMs: number): void {
    // Страховка от мусора: срок вне договорного диапазона не выдаёт кредита (команда уже проверена выше).
    if (!Number.isFinite(leaseMs) || leaseMs <= 0) return;
    this.#creditUntilMs = this.#environment.now() + Math.min(leaseMs, MAX_PLAY_CREDIT_LEASE_MS);
  }

  hasActiveCredit(): boolean {
    return this.#environment.now() < this.#creditUntilMs;
  }

  /** Передача элемента кончилась (Return, окно закрыто, замена): дальше запуск засчитывается только после нового ввода. */
  noteHandoffEnded(element: object): void {
    this.#handedBack.set(element, this.#environment.now());
  }

  /** Страница зовёт `play()`. Засчитан — запоминается метка, чтобы принять `loadedmetadata` позже. */
  judgePlayCall(element: object, audibility: ElementAudibility): ViewerPlayVerdict {
    const verdict = this.#judgeNow(element, audibility);
    if (verdict === "viewer_started") this.#creditedPlayCalls.set(element, this.#environment.now());
    return verdict;
  }

  /** Доверенное событие `play` (родные кнопки, `autoplay`): судим по обстановке сейчас. */
  judgeTrustedPlayEvent(element: object, audibility: ElementAudibility): ViewerPlayVerdict {
    return this.#judgeNow(element, audibility);
  }

  /**
   * Метаданные пришли. Годится, только если недавно был засчитанный `play()` этого элемента: сама активация
   * к этому времени могла истечь (метаданные идут секундами), а сам по себе `loadedmetadata` ничего не значит.
   */
  judgeLoadedMetadata(element: object, audibility: ElementAudibility): ViewerPlayVerdict {
    const markedAtMs = this.#creditedPlayCalls.get(element);
    const now = this.#environment.now();
    if (markedAtMs === undefined || now - markedAtMs > PLAY_CALL_MARK_WINDOW_MS) return "no_credited_play_call";
    const handedBackAtMs = this.#handedBack.get(element) ?? null;
    // Передача кончилась после засчитанного `play()`: тот запуск принадлежал уже закрытой передаче.
    if (handedBackAtMs !== null && handedBackAtMs >= markedAtMs) return "handed_back_without_new_input";
    const verdict = judgeViewerPlay({
      watch: this.#watch,
      // Клик доказан в момент `play()` — метка и есть его след.
      clickedThisVideo: true,
      creditActive: false,
      handedBackAtMs: null,
      lastTrustedInputAtMs: this.#environment.activation.lastTrustedInputAtMs(),
      audibility,
    });
    if (verdict === "viewer_started") this.#creditedPlayCalls.delete(element);
    return verdict;
  }

  /**
   * Кликнул ли человек по этому видео только что: последнее нажатие мыши моложе `VIEWER_CLICK_WINDOW_MS`,
   * и его точка лежит в прямоугольнике элемента (там у X, YouTube и большинства плееров кнопка Play и само
   * видео). Клик по лайку, соседнему посту или кнопке под видео сюда не попадает. Сбой чтения — «не кликнул».
   */
  #clickedOn(element: object): boolean {
    const press = this.#environment.activation.lastPointerPress();
    if (press === null || this.#environment.now() - press.atMs > VIEWER_CLICK_WINDOW_MS) return false;
    let rect: ViewportRect;
    try {
      rect = this.#environment.rectOf(element);
    } catch {
      return false;
    }
    return press.x >= rect.left && press.x <= rect.right && press.y >= rect.top && press.y <= rect.bottom;
  }

  #judgeNow(element: object, audibility: ElementAudibility): ViewerPlayVerdict {
    return judgeViewerPlay({
      watch: this.#watch,
      clickedThisVideo: this.#clickedOn(element),
      creditActive: this.hasActiveCredit(),
      handedBackAtMs: this.#handedBack.get(element) ?? null,
      lastTrustedInputAtMs: this.#environment.activation.lastTrustedInputAtMs(),
      audibility,
    });
  }
}
