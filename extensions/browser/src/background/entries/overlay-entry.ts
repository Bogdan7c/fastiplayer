// Вход «кнопка поверх видео» (сессия 10c). Контент-скрипт кадра шлёт `overlay_button_pressed` с меткой документа
// и ключом элемента; вкладку и фрейм называет браузер (`sender`), а не страница.
//
// Принимаем только своё сообщение (`sender.id === runtime.id`) из вкладки. Настройка выключена —
// передачи нет и сообщения нет: тихий отказ (кнопка уже должна была исчезнуть, это страховка гонки).

import { parseOverlayButtonPressed } from "../../shared/entry-validation.ts";
import type { HandOffTabOutcome, VideoRef } from "../../shared/handoff-contract.ts";
import type { EntrySettings } from "../../shared/entry-settings.ts";
import type { EntryReporter } from "./entry-reporter.ts";

/** Отправитель сообщения в той мере, в какой он нужен проверке. */
export interface OverlaySender {
  readonly id?: string | undefined;
  readonly tab?: { readonly id?: number | undefined } | undefined;
  readonly frameId?: number | undefined;
}

export interface OverlayEntryDependencies {
  readonly extensionId: string;
  readonly readSettings: () => Promise<EntrySettings>;
  readonly handOffTab: (tabId: number, video: VideoRef) => Promise<HandOffTabOutcome>;
  readonly reporter: EntryReporter;
  readonly note: (message: string, error: unknown) => void;
}

/** Слушатель `runtime.onMessage`. Ответа не даёт (`false`), итог человеку идёт плашкой. */
export function createOverlayEntryListener(dependencies: OverlayEntryDependencies): (message: unknown, sender: OverlaySender) => boolean {
  async function handle(tabId: number, video: VideoRef): Promise<void> {
    const settings = await dependencies.readSettings();
    if (!settings.overlay_button) return;
    let outcome: HandOffTabOutcome;
    try {
      outcome = await dependencies.handOffTab(tabId, video);
    } catch (error) {
      outcome = { kind: "failed", reason: `internal:${String(error)}` };
    }
    await dependencies.reporter.report(tabId, { kind: "outcome", outcome });
  }

  return (message, sender) => {
    const tabId = sender.tab?.id;
    if (sender.id !== dependencies.extensionId || tabId === undefined || sender.frameId === undefined) return false;
    const pressed = parseOverlayButtonPressed(message);
    if (pressed === undefined) return false;
    handle(tabId, { frame_id: sender.frameId, document: pressed.document, element: pressed.element }).catch((error: unknown) =>
      dependencies.note("overlay entry failed", error),
    );
    return false;
  };
}
