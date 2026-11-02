// Сборка MAIN-мира: оригиналы, хуки, журнал «памяти без копий», реестр элементов, канал участка 1
// и контроллер передачи. Всё — против переданного окна (в тестах — фейк), тень — через шов.
//
// Порядок на document_start (всё синхронно, до скриптов страницы):
//  1. снять оригиналы (двоичные геттеры, DOM, readyState, события и порты, console.debug,
//     crypto.getRandomValues) и выбрать метку документа;
//  2. поставить хуки MSE, `URL.createObjectURL`, `attachShadow`;
//  3. поставить слушатель рукопожатия участка 1.
// Нет `MediaSource` — хуки не ставятся вовсе; слушатель рукопожатия всё равно ставится, чтобы
// рукопожатие не досталось странице, а на вопросы фона был ответ («живого видео нет»).
// Глобалов не создаётся: всё живёт в замыканиях.

import { captureBinaryIntrinsics, type BinaryWindow } from "./binary-view.ts";
import { captureDomReaders, type DomWindow } from "./dom-readers.ts";
import { captureRandomFill, createDocumentTag, type RandomWindow } from "./document-tag.ts";
import { ElementRegistry, type RegistryWindow } from "./element-registry.ts";
import { HandoffController } from "./handoff-controller.ts";
import type { ShadowStarter } from "./handoff-seam.ts";
import { DocumentMemoryBudget, DOCUMENT_MEMORY_BUDGET_BYTES } from "./journal/memory-budget.ts";
import { captureMediaSourceReaders } from "./media-source-readers.ts";
import { installMseHook, type MseWindow } from "./mse-hook.ts";
import { MseTracker } from "./mse-tracker.ts";
import { createOddityReporter, type DebugSink } from "./oddity-reporter.ts";
import { captureChannelOriginals, listenForPageChannel, type ChannelWindow } from "./page-channel.ts";
import { VideoCandidates } from "./video-candidates.ts";

/** Окно, против которого работает MAIN-мир. */
export interface PageBridgeWindow extends MseWindow, DomWindow, RegistryWindow, ChannelWindow, BinaryWindow, RandomWindow {}

/** Внешние зависимости сборки. */
export interface PageBridgeDependencies {
  /** Шов с тенью (собирает `main.ts`). */
  readonly startShadow: ShadowStarter;
  /** Отладочная запись (оригинальный `console.debug`). */
  readonly debugSink: DebugSink;
  /** Потолок памяти журнала на документ (по умолчанию — `DOCUMENT_MEMORY_BUDGET_BYTES`). */
  readonly memoryBudgetBytes?: number;
}

/** Что сборка отдаёт наружу (только для тестов: странице не видно ничего). */
export interface PageBridge {
  readonly hooked: boolean;
  readonly tracker: MseTracker;
  readonly controller: HandoffController;
  readonly registry: ElementRegistry;
  readonly documentTag: string;
}

export function startPageBridge(win: PageBridgeWindow, dependencies: PageBridgeDependencies): PageBridge {
  const reportOddity = createOddityReporter(dependencies.debugSink);
  const binary = captureBinaryIntrinsics(win);
  const dom = captureDomReaders(win);
  const mediaSources = captureMediaSourceReaders(win);
  const channel = captureChannelOriginals(win);
  const documentTag = createDocumentTag(captureRandomFill(win));

  const budget = new DocumentMemoryBudget(dependencies.memoryBudgetBytes ?? DOCUMENT_MEMORY_BUDGET_BYTES);
  const registry = new ElementRegistry(dom, (candidate) => mediaSources.isMediaSource(candidate));
  const tracker = new MseTracker({
    budget,
    readyStateOf: (mediaSource) => mediaSources.readyStateOf(mediaSource),
    objectUrlOf: (mediaSource) => registry.objectUrlOf(mediaSource),
    reportOddity,
  });
  const candidates = new VideoCandidates(registry, tracker, dom, mediaSources, documentTag, reportOddity);
  const controller = new HandoffController({ registry, candidates, dom, binary, startShadow: dependencies.startShadow, reportOddity });
  tracker.setListener(controller);

  const hooked = win.MediaSource !== undefined && installMseHook(win, { observer: tracker, binary, reportOddity });
  if (hooked) registry.installHooks(win, reportOddity);

  listenForPageChannel(
    win,
    channel,
    {
      onConnected: (port) => controller.connect(port),
      onMessage: (message) => controller.handle(message),
    },
    reportOddity,
  );
  return { hooked, tracker, controller, registry, documentTag };
}
