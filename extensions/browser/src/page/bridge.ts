// Сборка MAIN-мира: оригиналы, хуки, журнал «памяти без копий», реестр элементов, канал участка 1
// и контроллер передачи. Всё — против переданного окна (в тестах — фейк), тень — через шов.
//
// Порядок на document_start (всё синхронно, до скриптов страницы):
//  1. снять оригиналы (двоичные геттеры, DOM, readyState, события и порты, console.debug,
//     crypto.getRandomValues) и выбрать метку документа;
//  2. поставить хуки MSE, `URL.createObjectURL`, `attachShadow` и (Chromium, 09b-C) обёртку
//     `Worker` — хук MSE внутри выделенных воркеров;
//  3. поставить слушатель рукопожатия участка 1 и позвать порт событием встречи READY
//     (договор page-channel-handshake.ts: порт приходит только к уже стоящему слушателю).
// Нет `MediaSource` — хуки не ставятся вовсе; слушатель рукопожатия всё равно ставится, чтобы
// рукопожатие не досталось странице, а на вопросы фона был ответ («живого видео нет»).
// Глобалов не создаётся: всё живёт в замыканиях.

import type { ScriptArrival } from "./script-arrival.ts";
import { captureBinaryIntrinsics, type BinaryWindow } from "./binary-view.ts";
import { captureDomReaders, type DomReaders, type DomWindow } from "./dom-readers.ts";
import { createForwardByteMeter } from "./forward-byte-meter.ts";
import { DrmMarks, type DrmWindow } from "./drm-marks.ts";
import { ProtectedMediaRelease } from "./protected-media-release.ts";
import { captureRandomFill, createDocumentTag, type RandomWindow } from "./document-tag.ts";
import { ElementRegistry, type RegistryWindow } from "./element-registry.ts";
import { HandoffController } from "./handoff-controller.ts";
import { NextVideoGuard } from "./next-video-guard.ts";
import type { HeldShadowStarter, PlayWatchSeam, ShadowStarter } from "./handoff-seam.ts";
import type { HandoffTap } from "./handoff-tap.ts";
import { DocumentMemoryBudget, DOCUMENT_MEMORY_BUDGET_BYTES } from "./journal/memory-budget.ts";
import { captureMediaSourceReaders } from "./media-source-readers.ts";
import { installMseHook, type MseWindow } from "./mse-hook.ts";
import { MseTracker } from "./mse-tracker.ts";
import { createOddityReporter, type DebugSink } from "./oddity-reporter.ts";
import { captureChannelOriginals, captureRendezvousOriginals, listenForPageChannel, type ChannelOriginals, type ChannelWindow, type PagePort, type RendezvousWindow } from "./page-channel.ts";
import { VideoCandidates } from "./video-candidates.ts";
import { isPointerQuery } from "./page-command-guard.ts";
import { capturePointerReaders, PointerVideo } from "./pointer-video.ts";
import { installWorkerCapture, type WorkerCaptureWindow, type WorkerChannelExtension, type WorkerHookSource, type WorkerMediaSources } from "./worker-capture/index.ts";

/** Окно, против которого работает MAIN-мир. */
export interface PageBridgeWindow extends MseWindow, DomWindow, RegistryWindow, ChannelWindow, BinaryWindow, RandomWindow, WorkerCaptureWindow, DrmWindow {
  /** Конструктор `Event` нужен встрече канала участка 1 (`RendezvousWindow`). */
  readonly Event: RendezvousWindow["Event"];
  /** Нужны для «видео под указателем» (10c); нет чего-то из них — вход по указателю не включается. */
  readonly MouseEvent?: { readonly prototype: object };
}

/** Внешние зависимости сборки. */
export interface PageBridgeDependencies {
  /** Шов с тенью (собирает `main.ts`). */
  readonly startShadow: ShadowStarter;
  /** Сторож следующего видео (сессия 09c2): тень на задержанном видео и шов с прототипами. */
  readonly startHeldShadow: HeldShadowStarter;
  readonly playWatch: PlayWatchSeam;
  /** Монотонные часы окна, снятые на document_start, мс. */
  readonly now: () => number;
  /** Отладочная запись (оригинальный `console.debug`). */
  readonly debugSink: DebugSink;
  /** Потолок памяти журнала на документ (по умолчанию — `DOCUMENT_MEMORY_BUDGET_BYTES`). */
  readonly memoryBudgetBytes?: number;
  /** Хук выделенного воркера (бандл и политика кодеков); без него обёртка `Worker` не ставится. */
  readonly workerHook?: WorkerHookSource;
  /** Встал ли скрипт до страницы или браузер вставил его в уже открытую вкладку (сессия 10d). */
  readonly scriptArrival: ScriptArrival;
  /** Подслушивание шагов передачи (только отладочная сборка записи потока страницы). */
  readonly handoffTap?: HandoffTap;
  /** Расширение каналов воркеров (только отладочная сборка записи потока страницы). */
  readonly workerChannelExtension?: WorkerChannelExtension;
}

/** Что сборка отдаёт наружу (только для тестов: странице не видно ничего). */
export interface PageBridge {
  readonly hooked: boolean;
  readonly tracker: MseTracker;
  readonly controller: HandoffController;
  readonly registry: ElementRegistry;
  readonly workerMediaSources: WorkerMediaSources;
  /** Метки DRM (только для тестов). */
  readonly drmMarks: DrmMarks;
  /** «Видео под указателем» (10c); `null` — окружение не даёт оригиналов, вход по указателю выключен. */
  readonly pointerVideo: PointerVideo | null;
  /** Обёртка `Worker` стоит (Chromium с хуком воркера). */
  readonly workersWrapped: boolean;
  readonly documentTag: string;
}

export function startPageBridge(win: PageBridgeWindow, dependencies: PageBridgeDependencies): PageBridge {
  const reportOddity = createOddityReporter(dependencies.debugSink);
  const binary = captureBinaryIntrinsics(win);
  const dom = captureDomReaders(win);
  const mediaSources = captureMediaSourceReaders(win);
  const channel = captureChannelOriginals(win);
  const rendezvous = captureRendezvousOriginals(win);
  const documentTag = createDocumentTag(captureRandomFill(win));

  const budget = new DocumentMemoryBudget(dependencies.memoryBudgetBytes ?? DOCUMENT_MEMORY_BUDGET_BYTES);
  const registry = new ElementRegistry(dom, (candidate) => mediaSources.isMediaSource(candidate));
  const tracker = new MseTracker({
    budget,
    readyStateOf: (mediaSource) => mediaSources.readyStateOf(mediaSource),
    objectUrlOf: (mediaSource) => registry.objectUrlOf(mediaSource),
    reportOddity,
  });
  // Реестр воркеров — всегда (классификатор узнаёт по нему handle); обёртка — только в Chromium.
  const workerCapture = installWorkerCapture(win, {
    hook: dependencies.workerHook ?? null,
    channel,
    binary,
    reportOddity,
    ...(dependencies.workerChannelExtension === undefined ? {} : { channelExtension: dependencies.workerChannelExtension }),
  });
  const workerMediaSources = workerCapture.registry;
  // Метки DRM — на document_start, как и остальные хуки (инвариант 4): иначе `setMediaKeys`, который
  // страница вызвала раньше нас, остался бы незамеченным.
  const drmMarks = new DrmMarks(win, dom, reportOddity);
  const candidates = new VideoCandidates(registry, tracker, dom, mediaSources, documentTag, reportOddity, workerMediaSources, drmMarks, dependencies.scriptArrival);
  const nextVideo = new NextVideoGuard({
    dom,
    registry,
    candidates,
    startHeldShadow: dependencies.startHeldShadow,
    seam: dependencies.playWatch,
    isProtected: (element) => drmMarks.isProtected(element),
    send: (event) => controller.sendToBackground(event),
    now: dependencies.now,
    reportOddity,
    documentEvents: win,
  });
  const controller: HandoffController = new HandoffController({
    registry,
    candidates,
    dom,
    binary,
    meter: createForwardByteMeter(),
    startShadow: dependencies.startShadow,
    workerMediaSources,
    reportOddity,
    protection: drmMarks,
    nextVideo,
    ...(dependencies.handoffTap === undefined ? {} : { tap: dependencies.handoffTap }),
  });
  // Защищённый MediaSource отпускает журнал (память), а передаваемый элемент при DRM отдаётся странице.
  const protectedRelease = new ProtectedMediaRelease(drmMarks, dom, candidates, reportOddity);
  tracker.setListener(protectedRelease.wrapListener(controller));
  workerMediaSources.setListener(controller);
  drmMarks.onProtected((element) => protectedRelease.releaseForElement(element));
  drmMarks.onProtected((element) => controller.onElementProtected(element));
  drmMarks.install(win);

  const hooked = win.MediaSource !== undefined && installMseHook(win, { observer: tracker, binary, reportOddity });
  if (hooked) registry.installHooks(win, reportOddity);

  const pointerVideo = installPointerVideo(win, channel, {
    documentTag,
    registry,
    dom,
    isInPlayer: (element) => controller.isHandingOffElement(element),
  });
  let pagePort: PagePort | null = null;
  listenForPageChannel(
    win,
    channel,
    rendezvous,
    {
      onConnected: (port) => {
        pagePort = port;
        controller.connect(port);
      },
      onMessage: (message) => {
        if (!isPointerQuery(message)) {
          controller.handle(message);
          return;
        }
        // Вопросы про указатель отвечает PointerVideo; нет окружения — честное «под указателем ничего нет».
        if (message.kind === "describe_right_clicked") {
          pagePort?.send({ kind: "right_clicked", request_id: message.request_id, video: pointerVideo?.describeRightClicked() ?? null });
        } else {
          pagePort?.send({ kind: "video_at_point_result", request_id: message.request_id, video: pointerVideo?.videoAtPoint(message.x, message.y) ?? null });
        }
      },
    },
    reportOddity,
  );
  return { hooked, tracker, controller, registry, workerMediaSources, drmMarks, pointerVideo, workersWrapped: workerCapture.wrapped, documentTag };
}

/** Что нужно сборке «видео под указателем» от остального моста. */
interface PointerVideoParts {
  readonly documentTag: string;
  readonly registry: ElementRegistry;
  readonly dom: DomReaders;
  readonly isInPlayer: (element: Element) => boolean;
}

/**
 * Собрать «видео под указателем» и поставить слушатель `contextmenu` на window в захвате (синхронно, на
 * document_start). Окружение без нужных оригиналов (старый браузер, фейк теста) — `null`, мост работает без входа.
 */
function installPointerVideo(win: PageBridgeWindow, channel: ChannelOriginals, parts: PointerVideoParts): PointerVideo | null {
  const readers = capturePointerReaders(win);
  if (readers === null) return null;
  const pointerVideo = new PointerVideo({
    documentTag: parts.documentTag,
    events: readers.events,
    tree: readers.tree,
    videoElements: () => parts.registry.findVideoElements(),
    keyOf: (element) => parts.registry.keyOf(element),
    isVideoElement: (candidate) => parts.dom.isVideoElement(candidate),
    isConnected: (node) => parts.dom.isConnected(node),
    rectOf: (element) => parts.dom.rectOf(element),
    isInPlayer: parts.isInPlayer,
  });
  channel.addListener(win, "contextmenu", (event) => pointerVideo.noteContextMenu(event), true);
  return pointerVideo;
}
