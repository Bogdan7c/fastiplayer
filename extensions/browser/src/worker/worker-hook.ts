// Сборка хука внутри выделенного воркера (сессия 09b, пакет C, решение 63).
//
// Хук едет в воркер внутри blob-скрипта обёртки MAIN-мира и выполняется РАНЬШЕ кода сайта.
// Порядок (всё синхронно):
//  1. снять оригиналы событий и портов и поставить слушатель рукопожатия — самым первым, чтобы
//     ни один сбой ниже не отдал наше рукопожатие сайту;
//  2. ответы про кодеки по запечённой политике документа (тот же модуль, что в MAIN-мире: HLS —
//     всегда «нет», семейства политики — «нет»);
//  3. хук MSE, учёт (`MseTracker`), журнал «памяти без копий» с потолком памяти (решение 176:
//     передача посреди просмотра Twitch требует уже скормленные куски);
//  4. сопоставление handle → MediaSource (геттер `handle` и `postMessage` воркера).
// Глобалов не создаётся. Ни одно исключение не уходит в код сайта.

import { captureBinaryIntrinsics, type BinaryIntrinsics, type BinaryWindow } from "../page/binary-view.ts";
import type { ChunkTap } from "../page/handoff-tap.ts";
import { installCodecAnswerOverrides, type CodecAnswerWindow } from "../page/codec-policy/answer-overrides.ts";
import { DocumentMemoryBudget, DOCUMENT_MEMORY_BUDGET_BYTES } from "../page/journal/memory-budget.ts";
import { captureMediaSourceReaders, type MediaSourceWindow } from "../page/media-source-readers.ts";
import { installMseHook, type MseWindow } from "../page/mse-hook.ts";
import { MseTracker } from "../page/mse-tracker.ts";
import { captureChannelOriginals, type ChannelWindow } from "../page/page-channel.ts";
import type { OddityReporter } from "../page/prototype-wrap.ts";
import { parseWorkerCommand } from "../shared/worker-channel-contract.ts";
import type { WorkerHookConfig } from "../shared/worker-hook-config.ts";
import { listenForWorkerChannel } from "./channel-handshake.ts";
import { installHandleTransferTracking, type HandleTransferScope } from "./handle-transfers.ts";
import { WorkerReportLink } from "./report-link.ts";
import { WorkerStreamController } from "./worker-stream-controller.ts";
import { createForwardByteMeter } from "../page/forward-byte-meter.ts";

/** Глобал выделенного воркера, против которого ставится хук (в тестах — фейк). */
export interface WorkerHookScope extends MseWindow, MediaSourceWindow, BinaryWindow, ChannelWindow, Omit<CodecAnswerWindow, "MediaSource">, HandleTransferScope {}

/** Что установка отдаёт наружу (точке входа и тестам: коду сайта не видно ничего). */
export interface WorkerHookInstallation {
  readonly hooked: boolean;
  readonly tracker: MseTracker;
  readonly link: WorkerReportLink;
  /**
   * Исходный скрипт сайта загружен (решение 192): донесение `site_script_loaded` в MAIN-мир — по
   * нему там исполняется отложенный `URL.revokeObjectURL` страницы. Зовёт текст blob-скрипта
   * обёртки (script-composer.ts) ровно после скрипта сайта; повторный вызов ничего не шлёт.
   * Никогда не бросает: вызов стоит в `finally` после `importScripts` сайта.
   */
  readonly reportSiteScriptLoaded: () => void;
}

/**
 * Расширение хука воркера (нейтральный шов): только отладочная сборка записи потока страницы
 * подставляет его. Хук воркера собирает его фабрикой, когда учёт и канал уже есть.
 */
export interface WorkerHookExtension {
  /** Подслушивание уходящих кусков байтов. */
  readonly chunkTap: ChunkTap;
  /** Команда MAIN-мира не распознана договором: `true` — расширение приняло её. */
  acceptCommand(data: unknown): boolean;
  /** Хук MSE встал: расширение ставит свои обёртки поверх (они снаружи хука и видят вызовы раньше). */
  hookInstalled(): void;
}

/** Что фабрика расширения получает от хука воркера. */
export interface WorkerHookExtensionContext {
  readonly scope: WorkerHookScope;
  readonly link: WorkerReportLink;
  readonly binary: BinaryIntrinsics;
  readonly tracker: MseTracker;
  readonly reportOddity: OddityReporter;
}

export type WorkerHookExtensionFactory = (context: WorkerHookExtensionContext) => WorkerHookExtension;

export function installWorkerHook(
  scope: WorkerHookScope,
  config: WorkerHookConfig,
  reportOddity: OddityReporter,
  createExtension?: WorkerHookExtensionFactory,
): WorkerHookInstallation {
  const channel = captureChannelOriginals(scope);
  const link = new WorkerReportLink(channel, reportOddity);
  // Контроллер нужен порту раньше, чем он создан: команды приходят только после рукопожатия.
  let controller: WorkerStreamController | null = null;
  // Расширение собирается позже (ему нужен учёт), а порт может ответить раньше — читаем через ссылку.
  let extension: WorkerHookExtension | null = null;
  listenForWorkerChannel(
    scope,
    channel,
    (port) => {
      channel.addListener(
        port,
        "message",
        (event) => {
          try {
            const command = parseWorkerCommand(channel.eventData(event));
            if (command !== undefined) controller?.handle(command);
            else if (extension?.acceptCommand(channel.eventData(event)) !== true) reportOddity("worker channel command is malformed");
          } catch {
            reportOddity("worker channel command handling failed");
          }
        },
        false,
      );
      channel.startPort(port);
      link.adopt(port);
    },
    reportOddity,
  );

  try {
    installCodecAnswerOverrides(scope, { current: () => config.codecPolicy }, reportOddity);
  } catch {
    reportOddity("worker codec answer overrides failed to install");
  }

  const binary = captureBinaryIntrinsics(scope);
  const mediaSources = captureMediaSourceReaders(scope);
  const budget = new DocumentMemoryBudget(DOCUMENT_MEMORY_BUDGET_BYTES);
  const tracker = new MseTracker({
    budget,
    readyStateOf: (mediaSource) => mediaSources.readyStateOf(mediaSource),
    // Blob-адресов для MediaSource воркера нет: к элементу он подключается только через handle.
    objectUrlOf: () => null,
    reportOddity,
  });
  extension = createExtension?.({ scope, link, binary, tracker, reportOddity }) ?? null;
  controller = new WorkerStreamController({
    link,
    binary,
    meter: createForwardByteMeter(),
    readyStateOf: (mediaSource) => mediaSources.readyStateOf(mediaSource),
    onSourceClose: (mediaSource, listener) => channel.addListener(mediaSource, "sourceclose", listener, false),
    reportOddity,
    ...(extension === null ? {} : { tap: extension.chunkTap }),
  });
  tracker.setListener(controller);
  const hooked = scope.MediaSource !== undefined && installMseHook(scope, { observer: tracker, binary, reportOddity });
  if (hooked) installHandleTransferTracking(scope, controller, reportOddity);
  if (hooked) extension?.hookInstalled();
  return { hooked, tracker, link, reportSiteScriptLoaded: siteScriptLoadedReporter(link, reportOddity) };
}

/** Донесение «скрипт сайта загружен» — один раз за жизнь воркера, без исключений наружу. */
function siteScriptLoadedReporter(link: WorkerReportLink, reportOddity: OddityReporter): () => void {
  let reported = false;
  return () => {
    if (reported) return;
    reported = true;
    try {
      link.send({ kind: "site_script_loaded" }, []);
    } catch {
      reportOddity("worker site script report failed");
    }
  };
}
