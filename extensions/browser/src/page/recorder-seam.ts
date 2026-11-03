// Шов отладочной записи потока страницы для `page/main.ts`.
//
// В ПРОДУКТОВОЙ сборке этот файл как есть: записи нет, `createPageRecording` возвращает `null`.
// Отладочная сборка (`npm run build -- --stream-recorder`) подменяет файл на
// `stream-recorder/page-seam-enabled.ts` (esbuild-плагин в scripts/build.ts) — так код записи
// попадает в бандл только по явному выбору сборки, а в продукте его нет ни в каком виде.

import type { HandoffTap } from "./handoff-tap.ts";
import type { MseTracker } from "./mse-tracker.ts";
import type { OddityReporter } from "./prototype-wrap.ts";
import type { WorkerChannelExtension } from "./worker-capture/index.ts";

/** Что `main.ts` нужно от записи. */
export interface PageRecordingSeam {
  readonly tap: HandoffTap;
  readonly channelExtension: WorkerChannelExtension;
  /** Поставить обёртки записи (после хуков моста и теневых переопределений). */
  install(tracker: MseTracker): void;
}

/** Продуктовая сборка: записи нет. */
export function createPageRecording(_win: Window & typeof globalThis, _reportOddity: OddityReporter): PageRecordingSeam | null {
  return null;
}
