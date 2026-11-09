// Наблюдение за вариантами страницы во фрейме (сессия 14b): озвучки, качества, следующая серия. Живёт в
// ИЗОЛИРОВАННОМ мире, как наблюдение за рекламой (`ad-watch.ts`), и устроено так же:
//
// - включается только арендой от фона (`arm_variant_watch`): до передачи вкладки страница и фон молчат
//   (решение 198). Срок вышел — наблюдатель снят, фон мог уснуть или умереть;
// - в фрейме работает первый адаптер, чьё `matches` истинно; на его корень ставится `MutationObserver`;
//   чтение сливается: не чаще раза в `VARIANT_READ_MIN_GAP_MS`, с обязательным хвостовым чтением;
// - чтение асинхронное (рука-функция ждёт ответ MAIN-мира), поэтому одновременно идёт одно чтение, а
//   перемены во время него дают ещё одно чтение сразу после;
// - донесение фону — при изменении описания и на каждом продлении, пока меню есть: молчание
//   `VARIANT_REPORT_SILENCE_MS` фон считает уходом меню. Не больше `VARIANT_REPORTS_PER_WINDOW` за окно,
//   лишние сливаются в последнее;
// - адаптер, бросивший исключение, — не ошибка экстеншена: описание считается пустым (`null`), фон убирает
//   меню у плеера, видео играет обычным перехватом (решение 282, инвариант «адаптер падает»).

import type { AdElement, AdFramePlace } from "../adapters/capabilities/ad-organs/contract.ts";
import type { PageVariantDescription, VariantActionOutcome } from "../adapters/capabilities/variants/contract.ts";
import type { SiteVariantAdapter } from "../adapters/registry/isolated-world.ts";
import { describeSiteAdapter, type SiteAdapterIdentity } from "../adapters/site-identity.ts";
import { VARIANT_WATCH_REPORT_KIND, type VariantCommand, type VariantWatchReport } from "../shared/variant-watch-contract.ts";
import type { RelayTimers } from "./relay.ts";

/** Чтение не чаще, чем раз в столько миллисекунд (слияние мутаций). Меню меняется по клику человека — не чаще. */
export const VARIANT_READ_MIN_GAP_MS = 250;

/** Окно подсчёта донесений. */
export const VARIANT_REPORT_WINDOW_MS = 1_000;

/** Сколько донесений фон получит от одного фрейма за окно: одно продление и пара настоящих перемен. */
export const VARIANT_REPORTS_PER_WINDOW = 4;

/** Что `MutationObserver` нам должен (подделка в тестах). */
export interface VariantMutationWatcher {
  observe(root: AdElement): void;
  disconnect(): void;
}

export interface VariantWatchDependencies {
  /** Все адаптеры вариантов; работает первый подходящий этому фрейму. */
  readonly adapters: readonly SiteVariantAdapter[];
  readonly place: AdFramePlace;
  readonly createWatcher: (onMutation: () => void) => VariantMutationWatcher;
  readonly timers: RelayTimers;
  /** Монотонные миллисекунды. */
  readonly now: () => number;
  /** Донесение фону, best-effort: следующее продление повторит. */
  readonly sendReport: (report: VariantWatchReport) => void;
  /** Сбой адаптера — только в журнал разработчика (паспорт без адресов и названий). */
  readonly noteAdapterFailure: (identity: SiteAdapterIdentity, error: unknown) => void;
}

export interface VariantWatch {
  /** Взять наблюдение в аренду на `leaseMs` (повторный вызов — продление). */
  arm(leaseMs: number): void;
  /** Исполнить команду плеера; нет подходящего адаптера — `no_such_variant`. */
  command(command: VariantCommand): Promise<VariantActionOutcome>;
  /** Идёт ли наблюдение (аренда жива и в этом фрейме есть адаптер). */
  isWatching(): boolean;
}

interface ActiveAdapter {
  readonly site: SiteVariantAdapter;
  readonly watcher: VariantMutationWatcher | undefined;
}

/** Чтение: обычное (по мутации или после команды) или продление аренды — оно же пульс донесения. */
type ReadReason = "change" | "renewal";

export function createVariantWatch(dependencies: VariantWatchDependencies): VariantWatch {
  const { timers, now } = dependencies;
  let leaseTimer: unknown;
  let leaseActive = false;
  let active: ActiveAdapter | undefined;
  let trailingReadTimer: unknown;
  let trailingReadActive = false;
  let lastReadAt = Number.NEGATIVE_INFINITY;
  let reading = false;
  let readAgain: ReadReason | undefined;
  /** Что фон знает от нас: `undefined` — ничего (или аренда начиналась заново). */
  let lastSentKey: string | undefined;
  /** Паспорт последнего адаптера, приславшего описание: с ним уходит и «меню больше нет». */
  let lastAdapterName = "";
  let flushTimer: unknown;
  let flushActive = false;
  let pendingReport: VariantWatchReport | undefined;
  const sentAt: number[] = [];

  // ---------------------------------------------------------------- адаптер фрейма

  /** Первый адаптер, подходящий фрейму сейчас. Сбой `matches` — «не подходит» и строка в журнал. */
  function matchingAdapter(): SiteVariantAdapter | undefined {
    return dependencies.adapters.find((site) => {
      try {
        return site.adapter.matches(dependencies.place);
      } catch (error) {
        dependencies.noteAdapterFailure(site.identity, error);
        return false;
      }
    });
  }

  function observe(site: SiteVariantAdapter): VariantMutationWatcher | undefined {
    try {
      const watcher = dependencies.createWatcher(() => onMutation());
      watcher.observe(site.adapter.observationRoot());
      return watcher;
    } catch (error) {
      // Без наблюдателя меню всё равно перечитывается на каждом продлении аренды — раз в секунду.
      dependencies.noteAdapterFailure(site.identity, error);
      return undefined;
    }
  }

  /** Привести наблюдение к подходящему адаптеру; `false` — в этом фрейме адаптера нет. */
  function syncAdapter(): boolean {
    const site = matchingAdapter();
    if (site === active?.site) return site !== undefined;
    active?.watcher?.disconnect();
    active = site === undefined ? undefined : { site, watcher: observe(site) };
    return active !== undefined;
  }

  // ---------------------------------------------------------------- аренда

  function arm(leaseMs: number): void {
    if (leaseActive) timers.clearTimeout(leaseTimer);
    leaseActive = true;
    leaseTimer = timers.setTimeout(stop, leaseMs);
    if (syncAdapter()) {
      void read("renewal");
      return;
    }
    // Адаптер ушёл (страница сменилась): фон должен узнать, что меню больше нет, если оно у него было.
    if (lastSentKey !== undefined) publish(reportOf(lastAdapterName, null), "change");
  }

  function stop(): void {
    leaseActive = false;
    timers.clearTimeout(leaseTimer);
    if (trailingReadActive) timers.clearTimeout(trailingReadTimer);
    trailingReadActive = false;
    if (flushActive) timers.clearTimeout(flushTimer);
    flushActive = false;
    pendingReport = undefined;
    active?.watcher?.disconnect();
    active = undefined;
    // Следующая аренда начнёт с чистого листа: фон к тому времени забыл фрейм по молчанию.
    lastSentKey = undefined;
  }

  // ---------------------------------------------------------------- чтение

  function onMutation(): void {
    if (!leaseActive || trailingReadActive) return;
    const waitMs = Math.max(0, lastReadAt + VARIANT_READ_MIN_GAP_MS - now());
    trailingReadActive = true;
    trailingReadTimer = timers.setTimeout(() => {
      trailingReadActive = false;
      void read("change");
    }, waitMs);
  }

  async function read(reason: ReadReason): Promise<void> {
    if (reading) {
      // Пульс важнее простой перемены: он обязан уйти, даже если описание не изменилось.
      readAgain = readAgain === "renewal" || reason === "renewal" ? "renewal" : "change";
      return;
    }
    const site = active?.site;
    if (!leaseActive || site === undefined) return;
    reading = true;
    lastReadAt = now();
    let description: PageVariantDescription | null;
    try {
      description = await site.adapter.describe();
    } catch (error) {
      dependencies.noteAdapterFailure(site.identity, error);
      description = null;
    }
    reading = false;
    // За время чтения аренда могла кончиться или адаптер смениться: такое описание уже ничьё.
    if (leaseActive && active?.site === site) {
      lastAdapterName = describeSiteAdapter(site.identity);
      publish(reportOf(lastAdapterName, description), reason);
    }
    const again = readAgain;
    readAgain = undefined;
    if (again !== undefined) void read(again);
  }

  // ---------------------------------------------------------------- донесения

  function descriptionKey(description: PageVariantDescription | null): string {
    return JSON.stringify(description);
  }

  function reportOf(adapter: string, description: PageVariantDescription | null): VariantWatchReport {
    return { kind: VARIANT_WATCH_REPORT_KIND, adapter, description };
  }

  function publish(report: VariantWatchReport, reason: ReadReason): void {
    const key = descriptionKey(report.description);
    // Пустое описание фону нужно, только если он слышал от нас непустое: иначе фреймы без меню молчат.
    if (report.description === null && (lastSentKey === undefined || lastSentKey === key)) {
      lastSentKey = lastSentKey ?? key;
      return;
    }
    const isHeartbeat = reason === "renewal" && report.description !== null;
    if (key === lastSentKey && !isHeartbeat) return;
    lastSentKey = key;
    pendingReport = report;
    flush();
  }

  function flush(): void {
    if (pendingReport === undefined) return;
    const currentMs = now();
    while (sentAt.length > 0 && currentMs - (sentAt[0] ?? 0) >= VARIANT_REPORT_WINDOW_MS) sentAt.shift();
    if (sentAt.length >= VARIANT_REPORTS_PER_WINDOW) {
      if (flushActive) return;
      flushActive = true;
      flushTimer = timers.setTimeout(() => {
        flushActive = false;
        flush();
      }, VARIANT_REPORT_WINDOW_MS - (currentMs - (sentAt[0] ?? currentMs)));
      return;
    }
    sentAt.push(currentMs);
    const report = pendingReport;
    pendingReport = undefined;
    dependencies.sendReport(report);
  }

  // ---------------------------------------------------------------- команды

  async function command(request: VariantCommand): Promise<VariantActionOutcome> {
    // Команда приходит фрейму, который прислал меню; аренда могла кончиться мгновение назад — адаптер найдём снова.
    const site = active?.site ?? matchingAdapter();
    if (site === undefined) return "no_such_variant";
    let outcome: VariantActionOutcome;
    try {
      outcome = request.kind === "select" ? await site.adapter.select(request.variant_kind, request.variant_id) : await site.adapter.next();
    } catch (error) {
      dependencies.noteAdapterFailure(site.identity, error);
      outcome = "failed";
    }
    // После нажатия меню страницы меняется: перечитать сразу, не дожидаясь мутаций и продления.
    if (leaseActive && outcome === "done") onMutation();
    return outcome;
  }

  return { arm, command, isWatching: () => leaseActive && active !== undefined };
}
