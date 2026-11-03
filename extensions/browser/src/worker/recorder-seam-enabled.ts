// Отладочный вариант шва записи воркера (подставляется вместо `recorder-seam.ts`, см. там).

import type { WorkerHookExtensionFactory } from "./worker-hook.ts";
import { createWorkerRecordingExtension } from "./worker-recording.ts";

export const workerRecordingExtension: WorkerHookExtensionFactory | undefined = createWorkerRecordingExtension;
