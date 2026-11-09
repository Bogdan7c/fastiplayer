// Наблюдение за рекламой во фрейме (сессия 09d). Живёт в ИЗОЛИРОВАННОМ мире: код страницы не может
// подменить наши помощники, а `page.js` (MAIN-мир) не растёт ни на байт.
//
// Включается только арендой от фона (`arm_ad_watch`), как сторож следующего видео: до передачи вкладки
// плееру страница и фон молчат (решение 198). Аренда короткая (фон продлевает раз в секунду, пока сеанс
// жив), срок вышел — наблюдатель снят, а фон умереть и забыть про фрейм может в любой момент.
//
// Что делает аренда, пока она жива:
// - на корень каждого подходящего читателя ставится `MutationObserver`; чтение сливается: не чаще раза
//   в `AD_READ_MIN_GAP_MS`, но с обязательным хвостовым чтением, чтобы последняя перемена не потерялась;
// - пока рекламы нет, мутации не читаются вовсе: фильм под тенью постоянно двигает плеер сайта, а
//   чтение стоит запросов раскладки (важно на старом железе). Рекламу замечает продление аренды, раз
//   в секунду; наблюдатель нужен, только чтобы отсчёт и кнопка не отставали, пока реклама идёт;
// - каждое продление аренды само читает состояние: это и начало рекламы, и страховка, если наблюдатель
//   что-то пропустил;
// - фону уходит донесение при изменении и на каждом продлении, пока реклама идёт: фон считает молчание
//   `AD_REPORT_SILENCE_MS` концом рекламы (iframe IMA просто удаляют, сказать «конец» некому);
// - страница, мигающая текстом, не заставит нас спамить: не больше `AD_REPORTS_PER_WINDOW` донесений
//   в окно, лишние сливаются в последнее.
//
// requestAnimationFrame не используется нигде: в скрытой вкладке он не вызывается. Таймеры и сам
// наблюдатель мутаций работают и там (таймеры скрытой вкладки замедлены до раза в секунду — для
// нас это всего лишь более редкое хвостовое чтение).
//
// Ошибка читателя — не ошибка передачи (мир страницы ненадёжен по определению): ловим её на границе,
// считаем «плашки нет» и идём дальше. Видео при этом продолжает играть.

import type { PageAdState } from "../protocol/index.ts";
import type { AdElement, AdFramePlace, AdOrgans, PressSkipOutcome } from "../adapters/registry/isolated-world.ts";
import { AD_WATCH_REPORT_KIND, type AdWatchReport } from "../shared/ad-watch-contract.ts";
import type { RelayTimers } from "./relay.ts";

/** Чтение не чаще, чем раз в столько миллисекунд (слияние мутаций). */
export const AD_READ_MIN_GAP_MS = 100;

/** Окно подсчёта донесений. */
export const AD_REPORT_WINDOW_MS = 1_000;

/** Сколько донесений фон получит от одного фрейма за окно. */
export const AD_REPORTS_PER_WINDOW = 8;

/** Что `MutationObserver` нам должен: следить за корнем и отключаться. Подделка в тестах. */
export interface AdMutationWatcher {
  observe(root: AdElement): void;
  disconnect(): void;
}

export interface AdWatchDependencies {
  /** Все читатели; наблюдаются только те, чьё `matches` истинно для этого фрейма. */
  readonly organs: readonly AdOrgans[];
  readonly place: AdFramePlace;
  /** Новый наблюдатель; `onMutation` он зовёт при любой подходящей перемене в корне. */
  readonly createWatcher: (onMutation: () => void) => AdMutationWatcher;
  readonly timers: RelayTimers;
  /** Монотонные миллисекунды. */
  readonly now: () => number;
  /** Донесение фону. Доставка best-effort: отказ — дело отправителя, следующее продление повторит. */
  readonly sendReport: (report: AdWatchReport) => void;
  /** Сбой читателя: только в журнал разработчика. */
  readonly reportReaderFailure: (error: unknown) => void;
}

export interface AdWatch {
  /** Взять наблюдение в аренду на `leaseMs` (повторный вызов — продление). */
  arm(leaseMs: number): void;
  /** Нажать кнопку пропуска; работает и без аренды: фон просит нажать то, о чём ему донесли. */
  pressSkip(): PressSkipOutcome;
  /** Идёт ли наблюдение (аренда жива и в этом фрейме есть что читать). */
  isWatching(): boolean;
}

interface Observation {
  readonly watcher: AdMutationWatcher;
  readonly root: AdElement;
}

const NO_AD: PageAdState = { kind: "none" };
const stateKey = (state: PageAdState): string => JSON.stringify(state);

export function createAdWatch(dependencies: AdWatchDependencies): AdWatch {
  const { timers, now } = dependencies;
  const observations = new Map<AdOrgans, Observation>();
  let leaseTimer: unknown;
  let leaseActive = false;
  let trailingReadTimer: unknown;
  let trailingReadActive = false;
  let flushTimer: unknown;
  let flushActive = false;
  let lastReadAt = Number.NEGATIVE_INFINITY;
  /** Шла ли реклама при последнем чтении: только тогда мутации стоят чтения. */
  let adSeenAtLastRead = false;
  /** Что фон знает от нас сейчас: молчание значит «нет рекламы». */
  let lastSentKey = stateKey(NO_AD);
  /** Состояние, которое ждёт своей очереди на отправку (лимит донесений). */
  let pendingState: PageAdState | undefined;
  const sentAt: number[] = [];

  // ---------------------------------------------------------------- читатели

  /** Читатели, подходящие этому фрейму прямо сейчас. Сбой `matches` — «не подходит». */
  function matchingOrgans(): AdOrgans[] {
    return dependencies.organs.filter((organs) => {
      try {
        return organs.matches(dependencies.place);
      } catch (error) {
        dependencies.reportReaderFailure(error);
        return false;
      }
    });
  }

  /** Что страница показывает сейчас: первое `showing` из подходящих читателей, иначе «рекламы нет». */
  function readState(): PageAdState {
    for (const organs of matchingOrgans()) {
      try {
        const state = organs.readAdState();
        if (state.kind === "showing") return state;
      } catch (error) {
        dependencies.reportReaderFailure(error);
      }
    }
    return NO_AD;
  }

  // ---------------------------------------------------------------- наблюдатели

  function dropObservation(organs: AdOrgans): void {
    observations.get(organs)?.watcher.disconnect();
    observations.delete(organs);
  }

  /** Привести наблюдателей в соответствие с фреймом: корень мог смениться (страница перестроила плеер). */
  function syncObservations(): void {
    const matching = matchingOrgans();
    for (const organs of [...observations.keys()]) {
      if (!matching.includes(organs)) dropObservation(organs);
    }
    for (const organs of matching) {
      let root: AdElement;
      try {
        root = organs.observationRoot();
      } catch (error) {
        dependencies.reportReaderFailure(error);
        continue;
      }
      if (observations.get(organs)?.root === root) continue;
      dropObservation(organs);
      const watcher = dependencies.createWatcher(requestRead);
      watcher.observe(root);
      observations.set(organs, { watcher, root });
    }
  }

  // ---------------------------------------------------------------- чтение и донесения

  function cancelTrailingRead(): void {
    if (trailingReadActive) timers.clearTimeout(trailingReadTimer);
    trailingReadActive = false;
  }

  /** Мутация: прочитать сейчас или, если читали только что, — один раз позже, когда окно слияния кончится. */
  function requestRead(): void {
    // Мутация, долетевшая уже после снятия аренды, ничего не читает. Вне рекламы её тоже не читаем:
    // начало рекламы заметит продление аренды.
    if (!leaseActive || trailingReadActive || !adSeenAtLastRead) return;
    const wait = lastReadAt + AD_READ_MIN_GAP_MS - now();
    if (wait <= 0) {
      readNow({ renewal: false });
      return;
    }
    trailingReadActive = true;
    trailingReadTimer = timers.setTimeout(() => {
      trailingReadActive = false;
      readNow({ renewal: false });
    }, wait);
  }

  function readNow(options: { readonly renewal: boolean }): void {
    lastReadAt = now();
    cancelTrailingRead();
    const state = readState();
    adSeenAtLastRead = state.kind === "showing";
    publish(state, options.renewal);
  }

  /**
   * Решает, нужно ли донесение: изменение — да; продление при идущей рекламе — да (это «я жив» для
   * фона); всё остальное молчание.
   */
  function publish(state: PageAdState, renewal: boolean): void {
    const changed = stateKey(state) !== lastSentKey;
    if (!changed && !(renewal && state.kind === "showing")) {
      pendingState = undefined;
      return;
    }
    pendingState = state;
    flushPending();
  }

  function cancelFlush(): void {
    if (flushActive) timers.clearTimeout(flushTimer);
    flushActive = false;
  }

  /** Отправить ожидающее состояние, если лимит донесений позволяет; иначе — когда окно освободится. */
  function flushPending(): void {
    if (pendingState === undefined || flushActive) return;
    const current = now();
    while (sentAt.length > 0 && (sentAt[0] ?? 0) <= current - AD_REPORT_WINDOW_MS) sentAt.shift();
    if (sentAt.length >= AD_REPORTS_PER_WINDOW) {
      const wait = (sentAt[0] ?? current) + AD_REPORT_WINDOW_MS - current;
      flushActive = true;
      flushTimer = timers.setTimeout(() => {
        flushActive = false;
        flushPending();
      }, wait);
      return;
    }
    const state = pendingState;
    pendingState = undefined;
    sentAt.push(current);
    lastSentKey = stateKey(state);
    dependencies.sendReport({ kind: AD_WATCH_REPORT_KIND, ad: state });
  }

  // ---------------------------------------------------------------- аренда

  function stop(): void {
    for (const organs of [...observations.keys()]) dropObservation(organs);
    cancelTrailingRead();
    cancelFlush();
    if (leaseActive) timers.clearTimeout(leaseTimer);
    leaseActive = false;
    pendingState = undefined;
    // Фон при молчании уже считает «рекламы нет»; новая аренда начнёт с чистого листа.
    lastSentKey = stateKey(NO_AD);
    adSeenAtLastRead = false;
    sentAt.length = 0;
  }

  function arm(leaseMs: number): void {
    if (leaseActive) timers.clearTimeout(leaseTimer);
    leaseActive = true;
    leaseTimer = timers.setTimeout(stop, leaseMs);
    syncObservations();
    if (observations.size === 0) {
      // Читать в этом фрейме нечего: наблюдателей нет и держать аренду незачем.
      stop();
      return;
    }
    readNow({ renewal: true });
  }

  function pressSkip(): PressSkipOutcome {
    let outcome: PressSkipOutcome = "no_button";
    for (const organs of matchingOrgans()) {
      let result: PressSkipOutcome;
      try {
        result = organs.pressSkip();
      } catch (error) {
        dependencies.reportReaderFailure(error);
        continue;
      }
      if (result === "pressed") return "pressed";
      if (result === "person_only") outcome = "person_only";
    }
    return outcome;
  }

  return { arm, pressSkip, isWatching: () => leaseActive };
}
