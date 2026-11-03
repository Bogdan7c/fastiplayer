// Отложенный `URL.revokeObjectURL` для blob-адресов, из которых созданы НАШИ обёрнутые воркеры
// (решение 192, находка 1 ревью 09b).
//
// Зачем. Сайты (встроенные воркеры Vite, worker-loader webpack) делают так:
//   const url = URL.createObjectURL(blob);
//   try { return new Worker(url); } finally { URL.revokeObjectURL(url); }
// Оригинальный конструктор `Worker` захватывает blob-адрес прямо в конструкторе, поэтому такой код
// работает. Наша обёртка подменяет скрипт: blob-адрес сайта читается позже, уже ВНУТРИ воркера
// (`importScripts(url)` / `import url`). Без этого модуля revoke страницы успевал бы раньше, и
// воркер сайта умирал бы с NetworkError.
//
// Как. `URL.revokeObjectURL` оборачивается. Пока blob-адрес «держит» наш воркер (`hold`), revoke
// страницы не исполняется сразу, а запоминается; настоящий revoke (оригиналом) исполняется, когда
// отпущено последнее удержание адреса: хук воркера доложил «скрипт сайта загружен»
// (`site_script_loaded`) или вышел срок `SITE_SCRIPT_LOAD_DEADLINE_MS`. Любой другой адрес
// отпускается сразу, как без нас: тем же оригиналом, с теми же `this` и аргументами.
//
// Что страница может заметить: только время. Отложенный адрес ещё живёт до донесения хука (обычно
// доли секунды): `fetch(url)` или ещё один `new Worker(url)` в это окно сработают, хотя без нас
// адрес был бы уже мёртв. Возвращаемое значение (`undefined`), исключения оригинала на неверных
// аргументах, имя и `length` обёртки — как у оригинала.
//
// Сосуществует с обёрткой `createObjectURL` реестра элементов (element-registry.ts): здесь
// оборачивается только `revokeObjectURL`, и оригинал зовётся через `Reflect.apply` — чужие обёртки
// (блокировщик до или после нас) проходятся в обоих порядках, как в prototype-wrap.ts.

import type { OddityReporter } from "../prototype-wrap.ts";
import type { TimeoutScheduler } from "../shadow/file-return.ts";

/**
 * Сколько держим revoke страницы, если хук не доложил «скрипт сайта загружен» (хук не встал,
 * рукопожатие сорвалось, модуль сайта бросил). Обычно донесение приходит за доли секунды; запуск
 * воркера на занятой машине бывает секундами. Минута с запасом покрывает запуск, а держать blob
 * дольше — лишь память, не поломка: поздний revoke сайту не вреден, ранний — смертелен воркеру.
 */
export const SITE_SCRIPT_LOAD_DEADLINE_MS = 60_000;

/** Удержание blob-адреса одним воркером. `release` идемпотентен. */
export interface BlobUrlHold {
  release(): void;
}

/** Что нужно отсрочке (снято на document_start). */
export interface BlobRevokeDeferralDependencies {
  /** Канонический blob-адрес строки (как его разберёт браузер); `null` — не blob-адрес. */
  readonly canonicalBlobUrl: (input: string) => string | null;
  readonly scheduleTimeout: TimeoutScheduler;
  readonly reportOddity: OddityReporter;
}

/** Учёт одного удерживаемого адреса. */
interface HeldUrl {
  /** Сколько наших воркеров ещё не загрузили скрипт сайта из этого адреса. */
  holders: number;
  /** Страница уже звала revoke — исполнить, когда отпустят все. */
  revokeRequested: boolean;
}

export class BlobRevokeDeferral {
  readonly #dependencies: BlobRevokeDeferralDependencies;
  readonly #held = new Map<string, HeldUrl>();
  /** Оригинальный revoke и его владелец (`URL`); `null` — обёртка не стоит. */
  #original: { readonly owner: object; readonly revoke: Function } | null = null;

  constructor(dependencies: BlobRevokeDeferralDependencies) {
    this.#dependencies = dependencies;
  }

  /**
   * Обернуть `owner.revokeObjectURL` (флаги свойства — как были). `false` — метода нет или его не
   * перезаписать: тогда отсрочки нет, и обёртка `Worker` ставиться не должна (иначе сайт с
   * «revoke сразу после конструктора» потерял бы воркер).
   */
  install(owner: object): boolean {
    const descriptor = Object.getOwnPropertyDescriptor(owner, "revokeObjectURL");
    const revoke: unknown = descriptor?.value;
    if (descriptor === undefined || typeof revoke !== "function" || (descriptor.configurable !== true && descriptor.writable !== true)) return false;
    this.#original = { owner, revoke };
    const deferral = this;
    // Метод из литерала объекта: имя «revokeObjectURL», как у оригинала, и он не конструктор.
    const wrapper = {
      revokeObjectURL(this: unknown, ...args: unknown[]): unknown {
        if (deferral.#deferPageRevoke(args)) return undefined;
        return Reflect.apply(revoke, this, args);
      },
    }.revokeObjectURL;
    Object.defineProperty(wrapper, "length", { value: (revoke as { readonly length: number }).length });
    Object.defineProperty(owner, "revokeObjectURL", { ...descriptor, value: wrapper });
    return true;
  }

  /**
   * Наш воркер будет читать скрипт сайта из `url`: revoke страницы до `release` (или до срока)
   * откладывается. Звать сразу после успешного конструктора воркера.
   */
  hold(url: string): BlobUrlHold {
    const canonical = this.#dependencies.canonicalBlobUrl(url) ?? url;
    const entry = this.#held.get(canonical) ?? { holders: 0, revokeRequested: false };
    entry.holders += 1;
    this.#held.set(canonical, entry);
    let released = false;
    let cancelDeadline: () => void = () => {};
    const release = (): void => {
      if (released) return;
      released = true;
      cancelDeadline();
      this.#releaseOne(canonical);
    };
    cancelDeadline = this.#dependencies.scheduleTimeout(release, SITE_SCRIPT_LOAD_DEADLINE_MS);
    return { release };
  }

  /** Сколько адресов сейчас удерживается (для тестов и отладки). */
  get heldUrlCount(): number {
    return this.#held.size;
  }

  /**
   * Revoke страницы: `true` — адрес удерживается, revoke запомнен и будет исполнен позже. Никогда
   * не бросает (сбой разбора — revoke идёт оригиналу сразу, как без нас).
   */
  #deferPageRevoke(args: readonly unknown[]): boolean {
    try {
      if (this.#held.size === 0) return false;
      const candidate = args[0];
      // Только строки: объект с `toString` страницы разбирать нельзя (вызов его `toString` дважды
      // был бы заметен) — такой revoke идёт оригиналу как есть.
      if (typeof candidate !== "string") return false;
      const canonical = this.#dependencies.canonicalBlobUrl(candidate);
      const entry = canonical === null ? undefined : this.#held.get(canonical);
      if (entry === undefined) return false;
      entry.revokeRequested = true;
      return true;
    } catch {
      this.#dependencies.reportOddity("blob revoke could not be inspected");
      return false;
    }
  }

  /** Один воркер отпустил адрес; последний — исполняет запомненный revoke страницы. */
  #releaseOne(canonical: string): void {
    const entry = this.#held.get(canonical);
    if (entry === undefined) return;
    entry.holders -= 1;
    if (entry.holders > 0) return;
    this.#held.delete(canonical);
    const original = this.#original;
    if (!entry.revokeRequested || original === null) return;
    try {
      Reflect.apply(original.revoke, original.owner, [canonical]);
    } catch {
      this.#dependencies.reportOddity("deferred blob revoke failed");
    }
  }
}
