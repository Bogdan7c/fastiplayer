// MSE в выделенном воркере (сессия 09b, пакет C): сборка на document_start.
//
// Реестр MediaSource воркеров создаётся всегда — по нему классификатор источника узнаёт handle в
// `srcObject` и честно говорит «не умеем», если воркер прошёл мимо нас. Обёртка `Worker` ставится
// ТОЛЬКО когда `MediaSource.canConstructInDedicatedWorker === true` (Chromium): в Firefox MSE в
// воркере нет, а обёртка там ломает Twitch (00c — `importScripts` blob-адреса страницы из нашего
// blob-воркера даёт NetworkError).
// Вместе с обёрткой ставится отсрочка `URL.revokeObjectURL` (blob-revoke-deferral.ts, решение
// 192): без неё обёртку ставить нельзя — сайт, отпускающий blob-адрес сразу после `new Worker`,
// потерял бы воркер. Не встала отсрочка — не ставится и обёртка (честное «не умеем»).

import type { CodecPolicy } from "../../shared/codec-policy.ts";
import { encodeWorkerHookConfig } from "../../shared/worker-hook-config.ts";
import type { BinaryIntrinsics } from "../binary-view.ts";
import type { ChannelOriginals } from "../page-channel.ts";
import { isObject, type OddityReporter } from "../prototype-wrap.ts";
import type { TimeoutScheduler } from "../shadow/file-return.ts";
import { BlobRevokeDeferral } from "./blob-revoke-deferral.ts";
import type { HookScript } from "./script-composer.ts";
import { WorkerMediaSources } from "./worker-media-sources.ts";
import { installWorkerWrapper, type WorkerWrapperOriginals } from "./worker-wrapper.ts";

export type { WorkerHandleResolution, WorkerMediaSourceListener, WorkerMediaSourceRef, WorkerStreamHandlers } from "./worker-media-sources.ts";
export { WorkerMediaSources } from "./worker-media-sources.ts";

/** Окно, против которого ставится захват (в тестах — фейк). Любого члена может не быть. */
export interface WorkerCaptureWindow {
  readonly Worker?: unknown;
  /** Статический `canConstructInDedicatedWorker` читается на старте. */
  readonly MediaSource?: { readonly prototype: object };
  readonly MediaSourceHandle?: { readonly prototype: object };
  readonly TrustedScriptURL?: { readonly prototype: object };
  readonly Blob?: { readonly prototype: object };
  readonly URL: object;
  readonly JSON?: { readonly stringify: (value: unknown) => string };
  readonly MessageChannel?: unknown;
  readonly origin?: unknown;
  readonly document: object;
  readonly Node: { readonly prototype: object };
  readonly setTimeout: (callback: () => void, delayMs: number) => unknown;
  readonly clearTimeout: (handle: never) => void;
}

/** Хук воркера: текст бандла и чтение действующей политики кодеков документа. */
export interface WorkerHookSource {
  readonly source: string;
  readonly policy: { current(): CodecPolicy };
}

/** Что нужно сборке. */
export interface WorkerCaptureDependencies {
  /** `null` — без хука (тесты моста, сборка без бандла воркера): обёртка не ставится. */
  readonly hook: WorkerHookSource | null;
  readonly channel: ChannelOriginals;
  readonly binary: BinaryIntrinsics;
  readonly reportOddity: OddityReporter;
}

/** Итог сборки. */
export interface WorkerCapture {
  readonly registry: WorkerMediaSources;
  /** Обёртка `Worker` стоит. */
  readonly wrapped: boolean;
}

export function installWorkerCapture(win: WorkerCaptureWindow, dependencies: WorkerCaptureDependencies): WorkerCapture {
  const { reportOddity } = dependencies;
  const isMediaSourceHandle = prototypeCheck(win.MediaSourceHandle?.prototype);
  const registry = new WorkerMediaSources({
    isMediaSourceHandle,
    binary: dependencies.binary,
    scheduleTimeout: captureTimeoutScheduler(win),
    reportOddity,
  });
  const hook = dependencies.hook;
  if (hook === null || !canConstructMediaSourceInWorker(win)) return { registry, wrapped: false };
  try {
    const originals = captureWrapperOriginals(win, isMediaSourceHandle);
    if (originals === null) {
      reportOddity("worker wrapper originals are missing");
      return { registry, wrapped: false };
    }
    const revokeDeferral = new BlobRevokeDeferral({
      canonicalBlobUrl: (input) => {
        const resolved = originals.resolveUrl(input, originals.documentBaseUrl());
        return resolved !== null && resolved.protocol === BLOB_PROTOCOL ? resolved.href : null;
      },
      scheduleTimeout: captureTimeoutScheduler(win),
      reportOddity,
    });
    if (!revokeDeferral.install(win.URL)) {
      reportOddity("blob revoke could not be deferred; workers are not wrapped");
      return { registry, wrapped: false };
    }
    const hookScript = (): HookScript => ({ source: hook.source, configLiteral: encodeWorkerHookConfig({ codecPolicy: hook.policy.current() }) });
    const wrapped = installWorkerWrapper(win, { originals, channel: dependencies.channel, registry, revokeDeferral, hookScript, reportOddity });
    if (!wrapped) reportOddity("worker constructor could not be wrapped");
    return { registry, wrapped };
  } catch {
    reportOddity("worker wrapper failed to install");
    return { registry, wrapped: false };
  }
}

/** Схема blob-адреса (строчными, с двоеточием — как `URL.protocol`). */
const BLOB_PROTOCOL = "blob:";

/** Условие решения 63: MSE можно создать в выделенном воркере. */
function canConstructMediaSourceInWorker(win: WorkerCaptureWindow): boolean {
  try {
    return isObject(win.MediaSource) && (win.MediaSource as { readonly canConstructInDedicatedWorker?: unknown }).canConstructInDedicatedWorker === true;
  } catch {
    return false;
  }
}

/** `prototype` в цепочке прототипов объекта (как `instanceof`, без подменяемых глобалов). */
function prototypeCheck(prototype: object | undefined): (candidate: unknown) => boolean {
  const isPrototypeOf = Object.prototype.isPrototypeOf;
  return (candidate) => prototype !== undefined && isObject(candidate) && Reflect.apply(isPrototypeOf, prototype, [candidate]) === true;
}

function captureTimeoutScheduler(win: WorkerCaptureWindow): TimeoutScheduler {
  const setTimer = win.setTimeout.bind(win);
  const clearTimer = win.clearTimeout.bind(win);
  return (callback, delayMs) => {
    const handle = setTimer(callback, delayMs);
    return () => clearTimer(handle as never);
  };
}

/** Найти аксессор по цепочке прототипов. */
function findGetter(prototype: object | null, name: string): ((target: unknown) => unknown) | undefined {
  for (let current = prototype; current !== null; current = Object.getPrototypeOf(current) as object | null) {
    const getter = Object.getOwnPropertyDescriptor(current, name)?.get;
    if (getter !== undefined) return (target) => Reflect.apply(getter, target, []);
  }
  return undefined;
}

/** Найти метод по цепочке прототипов. */
function findMethod(prototype: object | null, name: string): Function | undefined {
  for (let current = prototype; current !== null; current = Object.getPrototypeOf(current) as object | null) {
    const method: unknown = Object.getOwnPropertyDescriptor(current, name)?.value;
    if (typeof method === "function") return method;
  }
  return undefined;
}

/** Снять всё, что нужно обёртке; `null` — чего-то нет (обёртку тогда не ставим). */
function captureWrapperOriginals(win: WorkerCaptureWindow, isMediaSourceHandle: (candidate: unknown) => boolean): WorkerWrapperOriginals | null {
  const OriginalWorker = win.Worker;
  const BlobConstructor = win.Blob;
  const ChannelConstructor = win.MessageChannel;
  const UrlConstructor = win.URL;
  const stringify = win.JSON?.stringify;
  const documentOrigin = win.origin;
  if (typeof OriginalWorker !== "function" || typeof BlobConstructor !== "function" || typeof ChannelConstructor !== "function") return null;
  if (typeof UrlConstructor !== "function" || stringify === undefined || typeof documentOrigin !== "string") return null;
  const workerPrototype = (OriginalWorker as { readonly prototype?: unknown }).prototype;
  if (!isObject(workerPrototype)) return null;
  const postMessage = findMethod(workerPrototype, "postMessage");
  const createObjectUrl = (UrlConstructor as { readonly createObjectURL?: unknown }).createObjectURL;
  const revokeObjectUrl = (UrlConstructor as { readonly revokeObjectURL?: unknown }).revokeObjectURL;
  const urlPrototype = (UrlConstructor as { readonly prototype: object }).prototype;
  const href = findGetter(urlPrototype, "href");
  const origin = findGetter(urlPrototype, "origin");
  const protocol = findGetter(urlPrototype, "protocol");
  const baseUri = findGetter(win.Node.prototype, "baseURI");
  const channelPrototype = (ChannelConstructor as { readonly prototype: object }).prototype;
  // В браузере `port1/port2` — геттеры прототипа; где их нет (Node в тестах) — собственные свойства пары.
  const port1 = findGetter(channelPrototype, "port1") ?? ((pair: unknown) => (pair as { readonly port1: unknown }).port1);
  const port2 = findGetter(channelPrototype, "port2") ?? ((pair: unknown) => (pair as { readonly port2: unknown }).port2);
  if (postMessage === undefined || typeof createObjectUrl !== "function" || typeof revokeObjectUrl !== "function") return null;
  if (href === undefined || origin === undefined || protocol === undefined || baseUri === undefined) return null;
  const trustedScriptUrl = prototypeCheck(win.TrustedScriptURL?.prototype);
  const doc = win.document;
  return {
    Worker: OriginalWorker as WorkerWrapperOriginals["Worker"],
    postToWorker: (worker, message, transfer) => Reflect.apply(postMessage, worker, [message, transfer]),
    createPortPair: () => {
      const pair = Reflect.construct(ChannelConstructor, []) as object;
      return { ours: port1(pair) as object, theirs: port2(pair) as Transferable };
    },
    createScriptUrl: (scriptText) => {
      const blob = Reflect.construct(BlobConstructor, [[scriptText], { type: "text/javascript" }]) as object;
      return String(Reflect.apply(createObjectUrl, UrlConstructor, [blob]));
    },
    revokeScriptUrl: (url) => Reflect.apply(revokeObjectUrl, UrlConstructor, [url]),
    resolveUrl: (input, base) => {
      try {
        const parsed = Reflect.construct(UrlConstructor, [input, base]) as object;
        return { href: String(href(parsed)), origin: String(origin(parsed)), protocol: String(protocol(parsed)) };
      } catch {
        return null;
      }
    },
    documentBaseUrl: () => String(baseUri(doc)),
    documentOrigin,
    quote: (text) => Reflect.apply(stringify, win.JSON, [text]) as string,
    isTrustedScriptUrl: trustedScriptUrl,
    isMediaSourceHandle,
  };
}
