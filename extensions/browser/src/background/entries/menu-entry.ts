// Вход «правый клик по видео или по странице» (сессия 10c). Определяет только «какое видео»:
//  - в нужном фрейме (`info.frameId`) просит изолированный мир назвать видео последнего настоящего
//    правого клика (executeScript -> relay -> MAIN);
//  - видео не определилось (сайт забрал клик, клик мимо видео) — отдаём главное видео вкладки;
//  - дальше обычная команда передачи вкладки и итог в репортёр.

import type { RightClickedVideo } from "../../shared/entry-contract.ts";
import type { HandOffTabOutcome, VideoRef } from "../../shared/handoff-contract.ts";
import type { EntryReporter } from "./entry-reporter.ts";

/** Спросить фрейм вкладки, какое видео под последним правым кликом. Отклоняется, если спросить нельзя. */
export type AskRightClickedVideo = (tabId: number, frameId: number) => Promise<RightClickedVideo | null>;

export interface MenuEntryDependencies {
  readonly askRightClickedVideo: AskRightClickedVideo;
  readonly handOffTab: (tabId: number, video: VideoRef | undefined) => Promise<HandOffTabOutcome>;
  readonly reporter: EntryReporter;
  readonly note: (message: string, error: unknown) => void;
}

export interface MenuEntry {
  /** Клик по пункту «видео» или «страница» во фрейме `frameId` вкладки `tabId`. Не отклоняется. */
  playVideo(tabId: number, frameId: number): Promise<void>;
}

export function createMenuEntry(dependencies: MenuEntryDependencies): MenuEntry {
  /** Видео клика; `undefined` — главное видео вкладки. Сбой вопроса не отменяет вход: идём по главному. */
  async function videoOfClick(tabId: number, frameId: number): Promise<VideoRef | undefined> {
    try {
      const clicked = await dependencies.askRightClickedVideo(tabId, frameId);
      return clicked === null ? undefined : { frame_id: frameId, document: clicked.document, element: clicked.element };
    } catch (error) {
      dependencies.note("could not ask the frame which video was right-clicked, using the main video", error);
      return undefined;
    }
  }

  return {
    async playVideo(tabId, frameId) {
      const video = await videoOfClick(tabId, frameId);
      let outcome: HandOffTabOutcome;
      try {
        outcome = await dependencies.handOffTab(tabId, video);
      } catch (error) {
        outcome = { kind: "failed", reason: `internal:${String(error)}` };
      }
      await dependencies.reporter.report(tabId, { kind: "outcome", outcome });
    },
  };
}
