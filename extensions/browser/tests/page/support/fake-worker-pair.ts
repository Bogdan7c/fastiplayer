// Пара «страница ⇄ выделенный воркер» для тестов MSE в воркере (сессия 09b, пакет C).
//
// Добавляет фейковой странице (`createFakePage`) то, что нужно обёртке `Worker`: конструктор
// `Worker`, `MediaSourceHandle`, `Blob` с текстом, `MessageChannel`, `JSON`, `origin` и статический
// `MediaSource.canConstructInDedicatedWorker`. Ставить ДО `startPageBridge`: обёртка снимает
// оригиналы на старте.
//
// Как «браузер» запускает воркер. Blob-адрес конструктора захватывается В КОНСТРУКТОРЕ (как у
// настоящего браузера): отпущенный до него адрес даёт воркер, который не загрузится. Асинхронно
// после конструктора «браузер» берёт захваченный blob. Если это наш скрипт (в нём параметр
// настройки хука), из него достаются литерал настройки и адрес скрипта сайта (классический:
// `importScripts(...)`, модульный: последний `import`), в фейковый глобал ставится НАСТОЯЩИЙ хук
// (`installWorkerHook`), затем «выполняется» скрипт сайта: его адрес читается ТОЛЬКО СЕЙЧАС, и
// отпущенный к этому мигу blob-адрес сайта даёт NetworkError (`failed:<адрес>` в `loadOrder`) —
// ровно так revoke сразу после конструктора убивал воркер под обёрткой без отсрочки. После скрипта
// сайта — донесение «скрипт сайта загружен» (как `finally` в тексте blob), и только потом раздаются
// сообщения, пришедшие воркеру раньше (очередь порта включается после скрипта).
// Любой другой адрес (воркер мимо обёртки) запускается без хука.
// Сообщения воркера → странице: handle воркера заменяется handle главного потока (зеркало), всё
// остальное — структурное клонирование; события доверенные (`isTrusted`).

import { decodeWorkerHookConfig, WORKER_HOOK_CONFIG_PARAMETER } from "../../../src/shared/worker-hook-config.ts";
import { SITE_SCRIPT_LOAD_DEADLINE_MS } from "../../../src/page/worker-capture/blob-revoke-deferral.ts";
import { installWorkerHook } from "../../../src/worker/worker-hook.ts";
import { createFakeWorkerScope, type FakeWorkerScope } from "../../worker/support/fake-worker-scope.ts";
import { FAKE_PAGE_URL, type FakePage } from "./fake-page.ts";

export interface WorkerPairOptions {
  /** `MediaSource.canConstructInDedicatedWorker` (`undefined` — свойства нет вовсе). */
  readonly canConstructInDedicatedWorker?: boolean | undefined;
}

/** Blob с текстом, читаемым синхронно. */
export class FakeBlob {
  readonly text: string;
  readonly type: string;
  constructor(parts: readonly unknown[], options?: { type?: string }) {
    this.text = parts.map(String).join("");
    this.type = options?.type ?? "";
  }
}

/** Запущенный «браузером» воркер. */
export interface RunningWorker {
  readonly worker: EventTarget;
  readonly url: string;
  readonly options: unknown;
  readonly fake: FakeWorkerScope;
  /** Что выполнилось в воркере по порядку: "hook", затем адрес скрипта сайта (`failed:<адрес>` — не загрузился). */
  readonly loadOrder: string[];
  /** Литерал настройки из blob-скрипта (`null` — воркер без хука). */
  configLiteral: string | null;
  /** Странности хука воркера. */
  readonly oddities: string[];
}

/** Все пары портов, созданные через фейковый `MessageChannel` (закрываются после теста). */
const openChannels: MessageChannel[] = [];

/** `MessageChannel`, чьи порты тест закроет сам: иначе открытые порты Node держат процесс. */
class ClosableMessageChannel extends MessageChannel {
  constructor() {
    super();
    openChannels.push(this);
  }
}

/** Закрыть все порты, созданные обёрткой и хуком (зовётся в `afterEach`). */
export function closeWorkerChannels(): void {
  for (const channel of openChannels.splice(0)) {
    channel.port1.close();
    channel.port2.close();
  }
}

/** Сколько ждать шага «браузера» (запуск воркера, доставка сообщения), мс. */
const BROWSER_STEP_MS = 0;

export function addWorkerSupport(page: FakePage, options: WorkerPairOptions = { canConstructInDedicatedWorker: true }) {
  class FakeMediaSourceHandle {}
  const created: RunningWorker[] = [];
  /** Если задано и вернуло ошибку — конструктор `Worker` бросает её (CSP, Trusted Types). */
  const constructorFailure: { reject: ((url: string) => Error | null) | null } = { reject: null };
  /**
   * «Код сайта» воркера: зовётся в миг выполнения скрипта сайта (ПОСЛЕ хука, как в браузере) — тут
   * тест ставит слушатели сайта в воркере. `pageDeliveryDelayMs` — задержка доставки сообщений
   * воркера странице (так тест меняет порядок «донесение хука / сообщение сайта»).
   */
  const behaviour: { onSiteScript: ((running: RunningWorker) => void) | null; pageDeliveryDelayMs: number; reportSiteScriptLoaded: boolean } = {
    onSiteScript: null,
    pageDeliveryDelayMs: BROWSER_STEP_MS,
    // `false` — хук «не доложил» (не встал, модуль сайта бросил): удержание отпустит только срок.
    reportSiteScriptLoaded: true,
  };
  /** Handle главного потока → MediaSource воркера (для подключения к элементу). */
  const workerMediaSourceOf = new WeakMap<object, InstanceType<FakeWorkerScope["FakeMediaSource"]>>();
  const runningOf = new WeakMap<object, { running: RunningWorker; started: boolean; inbox: { data: unknown; ports: MessagePort[] }[] }>();

  class FakeWorker extends EventTarget {
    constructor(url: unknown, workerOptions?: unknown) {
      super();
      const href = String(url);
      const failure = constructorFailure.reject?.(href) ?? null;
      if (failure !== null) throw failure;
      // Как браузер: blob-адрес захватывается в конструкторе (отпущенный позже — уже не помеха).
      const capturedEntry = captureEntry(href);
      const fake = createFakeWorkerScope();
      const running: RunningWorker = { worker: this, url: href, options: workerOptions, fake, loadOrder: [], configLiteral: null, oddities: [] };
      const state = { running, started: false, inbox: [] as { data: unknown; ports: MessagePort[] }[] };
      runningOf.set(this, state);
      created.push(running);
      fake.delivery.onPost = (message, transfer) => deliverToPage(this, fake, message, transfer);
      setTimeout(() => {
        startWorker(running, capturedEntry);
        state.started = true;
        for (const queued of state.inbox.splice(0)) fake.deliverToWorker(queued.data, queued.ports);
      }, BROWSER_STEP_MS);
    }
    postMessage(message: unknown, transfer: readonly unknown[] = []): void {
      const state = runningOf.get(this);
      if (state === undefined) throw new TypeError("Illegal invocation");
      const ports = transfer.filter((entry): entry is MessagePort => entry instanceof MessagePort);
      const buffers = transfer.filter((entry): entry is ArrayBuffer => entry instanceof ArrayBuffer);
      const data: unknown = structuredClone(message, { transfer: buffers });
      setTimeout(() => {
        if (state.started) state.running.fake.deliverToWorker(data, ports);
        else state.inbox.push({ data, ports });
      }, BROWSER_STEP_MS);
    }
    terminate(): void {}
  }

  /** Что стоит за адресом конструктора на миг конструктора (`dead` — отпущенный blob). */
  function captureEntry(href: string): unknown {
    if (!href.startsWith("blob:")) return href;
    return isRevoked(href) ? DEAD_BLOB : page.objectUrlSubjects.get(href);
  }

  function isRevoked(url: string): boolean {
    return page.revokedObjectUrls.includes(url);
  }

  /** «Браузер» выполняет скрипт воркера (`entry` — захваченное в конструкторе). */
  function startWorker(running: RunningWorker, entry: unknown): void {
    if (entry === DEAD_BLOB) {
      running.loadOrder.push(`failed:${running.url}`);
      return;
    }
    if (!(entry instanceof FakeBlob) || !isWrapperScript(entry.text)) {
      runSiteScript(running, running.url); // воркер мимо обёртки: только скрипт сайта
      return;
    }
    const isModule = (running.options as { type?: unknown } | undefined)?.type === "module";
    const hookText = isModule ? moduleHookText(entry.text) : entry.text;
    const siteUrl = isModule ? lastImport(entry.text) : importScriptsUrl(entry.text);
    running.configLiteral = configLiteralOf(hookText);
    const installation = installWorkerHook(running.fake.scope, decodeWorkerHookConfig(JSON.parse(running.configLiteral)), (what) => running.oddities.push(what));
    running.loadOrder.push("hook");
    // Адрес сайта читается только сейчас — внутри воркера, а не в конструкторе.
    if (siteUrl.startsWith("blob:") && isRevoked(siteUrl)) running.loadOrder.push(`failed:${siteUrl}`);
    else runSiteScript(running, siteUrl);
    if (behaviour.reportSiteScriptLoaded) installation.reportSiteScriptLoaded();
  }

  function runSiteScript(running: RunningWorker, url: string): void {
    running.loadOrder.push(url);
    behaviour.onSiteScript?.(running);
  }

  function moduleHookText(entryText: string): string {
    const hookUrl = firstImport(entryText);
    const hook = page.objectUrlSubjects.get(hookUrl);
    if (!(hook instanceof FakeBlob)) throw new Error(`module hook ${hookUrl} is not a blob`);
    return hook.text;
  }

  /** Сообщение воркера → объект Worker страницы (доверенное, через «браузер»). */
  function deliverToPage(worker: EventTarget, fake: FakeWorkerScope, message: unknown, _transfer: readonly unknown[]): void {
    const data = cloneAcross(message, fake);
    setTimeout(() => {
      const event = new MessageEvent("message", { data });
      Object.defineProperty(event, "isTrusted", { value: true });
      worker.dispatchEvent(event);
    }, behaviour.pageDeliveryDelayMs);
  }

  /** Клон сообщения воркера: его handle — зеркальный handle главного потока. */
  function cloneAcross(value: unknown, fake: FakeWorkerScope): unknown {
    if (value instanceof fake.FakeMediaSourceHandle) {
      const mirror = new FakeMediaSourceHandle();
      const mediaSource = fake.mediaSourceOfHandle.get(value);
      if (mediaSource !== undefined) workerMediaSourceOf.set(mirror, mediaSource);
      return mirror;
    }
    if (Array.isArray(value)) return value.map((entry) => cloneAcross(entry, fake));
    if (typeof value === "object" && value !== null && Object.getPrototypeOf(value) === Object.prototype) {
      return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, cloneAcross(entry, fake)]));
    }
    return structuredClone(value);
  }

  // Срок удержания blob-адреса сайта (минута) тест не ждёт: такие таймеры копятся здесь и
  // срабатывают только по `fireSiteScriptDeadlines()`; остальные таймеры окна — настоящие.
  const pageTimers = page.win as unknown as { setTimeout: (callback: () => void, delayMs: number) => unknown; clearTimeout: (handle: unknown) => void };
  const realSetTimeout = pageTimers.setTimeout;
  const realClearTimeout = pageTimers.clearTimeout;
  const siteScriptDeadlines = new Map<object, () => void>();
  Object.assign(page.win, {
    setTimeout: (callback: () => void, delayMs: number): unknown => {
      if (delayMs !== SITE_SCRIPT_LOAD_DEADLINE_MS) return realSetTimeout(callback, delayMs);
      const handle = {};
      siteScriptDeadlines.set(handle, callback);
      return handle;
    },
    clearTimeout: (handle: unknown): void => {
      if (typeof handle === "object" && handle !== null && siteScriptDeadlines.delete(handle)) return;
      realClearTimeout(handle);
    },
  });

  if (options.canConstructInDedicatedWorker !== undefined) {
    Object.defineProperty(page.FakeMediaSource, "canConstructInDedicatedWorker", { value: options.canConstructInDedicatedWorker, configurable: true });
  }
  Object.assign(page.win, {
    Worker: FakeWorker,
    MediaSourceHandle: FakeMediaSourceHandle,
    Blob: FakeBlob,
    MessageChannel: ClosableMessageChannel,
    JSON,
    origin: new URL(FAKE_PAGE_URL).origin,
  });

  return {
    FakeWorker,
    FakeMediaSourceHandle,
    created,
    constructorFailure,
    behaviour,
    /** Сколько сроков удержания blob-адреса сайта ещё стоит. */
    pendingSiteScriptDeadlines: (): number => siteScriptDeadlines.size,
    /** «Прошла минута»: сработать все сроки удержания. */
    fireSiteScriptDeadlines(): void {
      const callbacks = [...siteScriptDeadlines.values()];
      siteScriptDeadlines.clear();
      for (const callback of callbacks) callback();
    },
    /**
     * Как Twitch/IVS и hls.js: blob-адрес страницы со скриптом воркера, который грузит настоящий
     * скрипт по абсолютному адресу (классический — `importScripts`, модульный — `import`).
     */
    siteBlobUrl(scriptPath: string, kind: "classic" | "module" = "classic"): string {
      const absolute = JSON.stringify(new URL(scriptPath, FAKE_PAGE_URL).href);
      const text = kind === "module" ? `import ${absolute};\n` : `importScripts(${absolute});\n`;
      const pageUrl = page.win.URL as unknown as { createObjectURL(blob: unknown): string };
      return pageUrl.createObjectURL(new FakeBlob([text], { type: "text/javascript" }));
    },
    /** Как страница: поставить handle главного потока элементу; MediaSource воркера открывается. */
    attachHandle(video: { srcObject: unknown }, handle: object): void {
      video.srcObject = handle;
      workerMediaSourceOf.get(handle)?.testOpen();
    },
  };
}

export type WorkerPair = ReturnType<typeof addWorkerSupport>;

/** Текст нашей обёртки (классический скрипт с хуком или входной модуль), а не blob страницы. */
export function isWrapperScript(text: string): boolean {
  return text.includes(WORKER_HOOK_CONFIG_PARAMETER) || text.startsWith("import { fastiplayerSiteScriptLoaded }");
}

/** Отпущенный blob-адрес, захваченный конструктором. */
const DEAD_BLOB = Symbol("revoked blob");

/** Литерал настройки из `(function (…) {…})(<литерал>, <ссылка>);`. */
function configLiteralOf(hookText: string): string {
  const match = /\}\)\(("[a-z0-9,]*"), fastiplayerWorkerHookLinks\);\n/.exec(hookText);
  if (match?.[1] === undefined) throw new Error("hook text has no config literal");
  return match[1];
}

function importScriptsUrl(text: string): string {
  const match = /importScripts\(("(?:[^"\\]|\\.)*")\);/.exec(text);
  if (match?.[1] === undefined) throw new Error("classic worker text has no importScripts");
  return JSON.parse(match[1]) as string;
}

function imports(text: string): string[] {
  return [...text.matchAll(/^import ("(?:[^"\\]|\\.)*");$/gm)].map((match) => JSON.parse(match[1] ?? '""') as string);
}

/** Адрес модуля хука: `import { … } from "<адрес>";`. */
function firstImport(text: string): string {
  const match = /^import \{ \w+ \} from ("(?:[^"\\]|\\.)*");$/m.exec(text);
  if (match?.[1] === undefined) throw new Error("module entry has no hook import");
  return JSON.parse(match[1]) as string;
}

function lastImport(text: string): string {
  const last = imports(text).at(-1);
  if (last === undefined) throw new Error("module entry has no imports");
  return last;
}
