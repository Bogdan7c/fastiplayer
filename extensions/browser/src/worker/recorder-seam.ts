// Шов отладочной записи MSE воркера для `worker/main.ts`.
//
// В ПРОДУКТОВОЙ сборке этот файл как есть: расширения нет. Отладочная сборка подменяет его на
// `worker-recording.ts`-вариант (`worker/recorder-seam-enabled.ts`, esbuild-плагин в scripts/build.ts).

import type { WorkerHookExtensionFactory } from "./worker-hook.ts";

/** Продуктовая сборка: записи нет. */
export const workerRecordingExtension: WorkerHookExtensionFactory | undefined = undefined;
