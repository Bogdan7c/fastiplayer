// Обёртка конструктора `Worker` в MAIN-мире (сессия 09b, пакет C, решение 63; механизм подтвердил
// владелец, решение 188; что оборачивать — решение 192): хук MSE едет внутрь выделенного воркера.
//
// Оборачиваются ТОЛЬКО воркеры из blob-адреса того же происхождения (решение 192). Так делают
// плееры, которым это и нужно: Twitch (IVS) и hls.js собирают воркер из blob. У такого воркера
// базовый адрес и без нас blob-адрес, а живёт он по CSP документа — наша подмена для сайта ничего
// не меняет. Воркер из http(s)-адреса НЕ оборачивается: подмена сделала бы его базовым адресом наш
// blob (относительные `fetch`/`importScripts`/`new Worker`/`new URL(x, self.location)` и
// `scriptDirectory` Emscripten → сломанный wasm) и поставила бы его под CSP документа вместо
// заголовков его скрипта — обычный сайт сломался бы (решение 181: наш код на каждой странице
// прозрачен). `data:` (непрозрачное происхождение), `TrustedScriptURL` (наш адрес-строку такая
// страница отвергла бы), чужое происхождение — тоже оригиналу как есть. MediaSource необёрнутого
// воркера честно остаётся «не умеем» (`untracked_worker_media_source`).
//
// `new Worker(url, options)` страницы:
//  1. адрес разрешается от базового адреса документа; оборачиваем только `blob:` того же
//     происхождения;
//  2. blob-скрипт «хук + скрипт сайта» (`script-composer.ts`), создание оригинальным конструктором с
//     теми же `options` и тем же `new.target`;
//  3. создать не вышло (CSP `worker-src` без `blob:` для НАШЕГО blob Chromium отвергает прямо в
//     конструкторе, SecurityError) — создаём ОРИГИНАЛ с исходными аргументами: воркер страницы не
//     ломается никогда, а его MediaSource честно остаётся «не умеем»;
//  4. blob-адрес сайта удерживается (`blob-revoke-deferral.ts`): оригинал захватывает адрес в
//     конструкторе, а наш воркер читает его позже, и `URL.revokeObjectURL` сразу после
//     конструктора (Vite, worker-loader) без отсрочки убил бы воркер сайта. Удержание отпускает
//     донесение хука «скрипт сайта загружен» или срок;
//  5. первым сообщением воркеру — метка канала и порт (transfer); свой слушатель `message` на
//     объекте Worker ставится тут же, раньше любых слушателей страницы, и считает сообщения и
//     handle в них (ничего не гасит: сайт получает все свои сообщения как без нас).
//
// Прозрачность: `Worker.prototype` — тот же объект, `instanceof Worker` и наследование классом
// работают (`new.target` передаётся оригиналу), имя «Worker», `length` как у оригинала, результат —
// настоящий Worker. Не скрыто лишь одно: `Function.prototype.toString` обёртки — не «native code»
// (как и у остальных обёрток MAIN-мира).
//
// Известные ограничения (документированы в отчёте 09b):
//  - плеер, создающий MSE-воркер из http(s)-адреса, не передаётся (решение 192: честное «не умеем»);
//  - отложенный revoke страница может заметить только по времени (см. blob-revoke-deferral.ts);
//  - наш личный порт держит воркер живым, пока жив документ (сайт, бросивший воркер без
//    `terminate()`, не дождётся его сборки).
// SharedWorker не оборачивается: MSE в нём по стандарту не создать (отклонение от буквы решения 63).

import { WORKER_CHANNEL_HANDSHAKE } from "../../shared/worker-channel-contract.ts";
import type { ChannelOriginals } from "../page-channel.ts";
import { isObject, type OddityReporter } from "../prototype-wrap.ts";
import type { BlobUrlHold } from "./blob-revoke-deferral.ts";
import { scanForMediaSourceHandles } from "./handle-scan.ts";
import { classicWorkerScript, moduleEntryScript, moduleHookScript, type HookScript } from "./script-composer.ts";
import type { WorkerMediaSources } from "./worker-media-sources.ts";

/** Конструктор как значение (для `Reflect.construct`). */
type AnyConstructor = new (...args: unknown[]) => object;

/** Оригиналы окна, нужные обёртке (снимаются один раз на document_start). */
export interface WorkerWrapperOriginals {
  readonly Worker: AnyConstructor & { readonly prototype: object; readonly length: number };
  /** `Worker.prototype.postMessage` оригиналом. */
  readonly postToWorker: (worker: object, message: unknown, transfer: readonly Transferable[]) => void;
  /** Новая пара портов (`new MessageChannel()` оригиналом). */
  readonly createPortPair: () => { readonly ours: object; readonly theirs: Transferable };
  /** Blob с текстом JavaScript → blob-адрес. */
  readonly createScriptUrl: (scriptText: string) => string;
  readonly revokeScriptUrl: (url: string) => void;
  /** Разбор адреса относительно базы; `null` — адрес не разбирается (оригинал бросит сам). */
  readonly resolveUrl: (input: string, base: string) => { readonly href: string; readonly origin: string; readonly protocol: string } | null;
  /** Базовый адрес документа (`document.baseURI` оригинальным геттером). */
  readonly documentBaseUrl: () => string;
  /** Происхождение документа на document_start («null» — непрозрачное). */
  readonly documentOrigin: string;
  /** Литерал строки JavaScript (`JSON.stringify` оригиналом). */
  readonly quote: (text: string) => string;
  readonly isTrustedScriptUrl: (candidate: unknown) => boolean;
  readonly isMediaSourceHandle: (candidate: unknown) => boolean;
}

/** Что ещё нужно обёртке. */
export interface WorkerWrapperDependencies {
  readonly originals: WorkerWrapperOriginals;
  readonly channel: ChannelOriginals;
  readonly registry: WorkerMediaSources;
  /** Отсрочка revoke blob-адресов сайта, из которых читают наши воркеры (blob-revoke-deferral.ts). */
  readonly revokeDeferral: { hold(url: string): BlobUrlHold };
  /** Хук для подстановки — читается в миг создания воркера (политика кодеков могла прийти позже). */
  readonly hookScript: () => HookScript;
  readonly reportOddity: OddityReporter;
}

/** Единственная схема адреса скрипта, который оборачиваем (решение 192; строчными, с двоеточием). */
const WRAPPABLE_PROTOCOL = "blob:";

/** Непрозрачное происхождение (`self.origin` песочницы). */
const OPAQUE_ORIGIN = "null";

/** Тип модульного воркера (`WorkerOptions.type`). */
const MODULE_WORKER_TYPE = "module";

/** Подготовленный blob-скрипт для одного воркера. */
interface WrappedScript {
  /** Адрес, с которым создаётся воркер. */
  readonly entryUrl: string;
  /** Все НАШИ blob-адреса этого воркера (отпускаются, когда хук встал или создание сорвалось). */
  readonly blobUrls: readonly string[];
  /** Канонический blob-адрес скрипта сайта (его удерживаем от revoke страницы). */
  readonly siteUrl: string;
}

/**
 * Обёртка-конструктор (её и ставит `installWorkerWrapper`). Отдельно от установки — чтобы тесты
 * проверяли поведение без подмены глобала.
 */
export function createWorkerWrapper(dependencies: WorkerWrapperDependencies): Function {
  const { originals, reportOddity } = dependencies;
  const OriginalWorker = originals.Worker;

  const wrapper = function Worker(this: unknown, ...args: unknown[]): object {
    // Вызов без `new`: пусть бросит сам оригинал — с его собственным сообщением.
    if (new.target === undefined) return Reflect.apply(OriginalWorker, this, args) as object;
    const script = prepareScript(dependencies, args);
    if (script !== null) {
      let worker: object | null = null;
      try {
        worker = Reflect.construct(OriginalWorker, [script.entryUrl, ...args.slice(1)], new.target);
      } catch {
        // CSP или иной отказ blob-воркеру: страница получает свой воркер как без нас.
        releaseScript(originals, script);
        reportOddity("worker could not be wrapped; the original worker is used");
      }
      if (worker !== null) {
        attachChannel(dependencies, worker, script, holdSiteUrl(dependencies, script));
        return worker;
      }
    }
    return Reflect.construct(OriginalWorker, args, new.target);
  };
  Object.defineProperty(wrapper, "name", { value: OriginalWorker.name });
  Object.defineProperty(wrapper, "length", { value: OriginalWorker.length });
  Object.defineProperty(wrapper, "prototype", { value: OriginalWorker.prototype, writable: false, enumerable: false, configurable: false });
  Object.setPrototypeOf(wrapper, Object.getPrototypeOf(OriginalWorker));
  return wrapper;
}

/**
 * Поставить обёртку вместо `win.Worker` (флаги свойства — как были). `false` — свойства нет или оно
 * не перезаписывается (тогда воркеры страницы идут без хука, честно «не умеем»).
 */
export function installWorkerWrapper(win: object, dependencies: WorkerWrapperDependencies): boolean {
  const descriptor = Object.getOwnPropertyDescriptor(win, "Worker");
  if (descriptor === undefined || descriptor.value !== dependencies.originals.Worker || descriptor.configurable !== true) return false;
  const wrapper = createWorkerWrapper(dependencies);
  const prototype = dependencies.originals.Worker.prototype;
  const constructorDescriptor = Object.getOwnPropertyDescriptor(prototype, "constructor");
  Object.defineProperty(win, "Worker", { ...descriptor, value: wrapper });
  // `Worker.prototype.constructor === Worker`, как у нетронутого браузера.
  if (constructorDescriptor !== undefined) Object.defineProperty(prototype, "constructor", { ...constructorDescriptor, value: wrapper });
  return true;
}

/** Blob-скрипт для аргументов страницы; `null` — этот воркер не оборачиваем. */
function prepareScript(dependencies: WorkerWrapperDependencies, args: readonly unknown[]): WrappedScript | null {
  const { originals, reportOddity } = dependencies;
  try {
    const urlArgument = args[0];
    if (originals.isTrustedScriptUrl(urlArgument) || originals.documentOrigin === OPAQUE_ORIGIN) return null;
    const resolved = originals.resolveUrl(String(urlArgument), originals.documentBaseUrl());
    if (resolved === null || resolved.protocol !== WRAPPABLE_PROTOCOL || resolved.origin !== originals.documentOrigin) return null;
    const hook = dependencies.hookScript();
    const scriptUrl = originals.quote(resolved.href);
    if (workerTypeOf(args[1]) === MODULE_WORKER_TYPE) {
      const hookUrl = originals.createScriptUrl(moduleHookScript(hook));
      const entryUrl = originals.createScriptUrl(moduleEntryScript(originals.quote(hookUrl), scriptUrl));
      return { entryUrl, blobUrls: [hookUrl, entryUrl], siteUrl: resolved.href };
    }
    const entryUrl = originals.createScriptUrl(classicWorkerScript(hook, scriptUrl));
    return { entryUrl, blobUrls: [entryUrl], siteUrl: resolved.href };
  } catch {
    // Чужой «адрес» с бросающим toString, сбой Blob: оригинал разберётся с аргументами сам.
    reportOddity("worker script could not be prepared");
    return null;
  }
}

/** `options.type` страницы (читается один раз; не объект — классический воркер). */
function workerTypeOf(options: unknown): unknown {
  return isObject(options) ? (options as { readonly type?: unknown }).type : undefined;
}

function releaseScript(originals: WorkerWrapperOriginals, script: WrappedScript): void {
  for (const url of script.blobUrls) originals.revokeScriptUrl(url);
}

/**
 * Удержать blob-адрес сайта от revoke страницы до донесения «скрипт сайта загружен» (или срока).
 * Сбой удержания — не повод ломать воркер: тогда revoke страницы идёт сразу, как раньше.
 */
function holdSiteUrl(dependencies: WorkerWrapperDependencies, script: WrappedScript): BlobUrlHold | null {
  try {
    return dependencies.revokeDeferral.hold(script.siteUrl);
  } catch {
    dependencies.reportOddity("worker site script URL could not be held");
    return null;
  }
}

/**
 * Канал с хуком: свой слушатель сообщений на объекте Worker (счёт и handle), затем рукопожатие.
 * Сбой здесь воркер страницы не ломает: хук без рукопожатия отдаёт первое сообщение сайту, а
 * удержание адреса сайта отпустит срок.
 */
function attachChannel(dependencies: WorkerWrapperDependencies, worker: object, script: WrappedScript, siteUrlHold: BlobUrlHold | null): void {
  const { originals, channel: channelOriginals, registry, reportOddity } = dependencies;
  try {
    let released = false;
    const channel = registry.createChannel({
      originals: channelOriginals,
      onHookReady: () => {
        if (released) return;
        released = true;
        releaseScript(originals, script);
      },
      onSiteScriptLoaded: () => siteUrlHold?.release(),
      reportOddity,
    });
    const readers = { isMediaSourceHandle: originals.isMediaSourceHandle, isTraversable };
    const onMessage = (event: unknown): void => {
      try {
        if (!isTrustedEvent(event)) return;
        const messageOrdinal = channel.noteWorkerMessage();
        const scan = scanForMediaSourceHandles(channelOriginals.eventData(event), readers);
        if (scan.kind === "one") registry.noteHandleArrived(scan.handle, channel, messageOrdinal);
      } catch {
        reportOddity("worker message could not be inspected");
      }
    };
    // `messageerror` — сообщение воркера, которое не разобралось: в счёте оно тоже есть.
    const onMessageError = (event: unknown): void => {
      if (isTrustedEvent(event)) channel.noteWorkerMessage();
    };
    channelOriginals.addListener(worker, "message", onMessage, true);
    channelOriginals.addListener(worker, "messageerror", onMessageError, true);
    const ports = originals.createPortPair();
    channel.connect(ports.ours);
    originals.postToWorker(worker, WORKER_CHANNEL_HANDSHAKE, [ports.theirs]);
  } catch {
    reportOddity("worker channel could not be attached");
  }
}

/**
 * Событие от браузера, а не `dispatchEvent` страницы (иначе подделка сбила бы счёт сообщений).
 * `isTrusted` — собственное неподменяемое свойство события ([LegacyUnforgeable]).
 */
function isTrustedEvent(event: unknown): boolean {
  return isObject(event) && (event as { readonly isTrusted?: unknown }).isTrusted === true;
}

/** Обходим только простые объекты и массивы этого мира (их и даёт структурное клонирование). */
const OBJECT_PROTOTYPE: object = Object.prototype;
const ARRAY_PROTOTYPE: object = Array.prototype;
function isTraversable(candidate: object): boolean {
  const prototype: unknown = Object.getPrototypeOf(candidate);
  return prototype === OBJECT_PROTOTYPE || prototype === ARRAY_PROTOTYPE || prototype === null;
}
