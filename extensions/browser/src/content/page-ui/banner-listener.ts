// Приём сообщения «покажи плашку» от фона. Здесь решается, КОМУ верим:
//  - только своему фону: `sender.id === runtime.id` и нет `sender.tab` (у фона вкладки нет; у контент-скрипта
//    любой вкладки и страницы с `externally_connectable` она была бы);
//  - только верхний фрейм: плашка одна на вкладку, во вложенных фреймах (реклама, плееры) её нет;
//  - форма сообщения проверена пограничником (`parseShowEntryResult`), мусор молча игнорируется.
import { parseShowEntryResult } from "../../shared/entry-validation.ts";
import type { BannerController } from "./banner-controller.ts";

/** Что мы читаем у отправителя. */
export interface MessageSenderFacts {
  readonly id?: string | undefined;
  readonly tab?: unknown;
}

export interface BannerListenerDependencies {
  /** `runtime.id` расширения. */
  readonly ownExtensionId: string;
  /** Этот документ — верхний фрейм вкладки (фрейм 0). */
  readonly isTopFrame: boolean;
  readonly banner: BannerController;
}

/** Слушатель для `runtime.onMessage`; ничего не отвечает (возвращает `undefined`). */
export function createBannerMessageListener(dependencies: BannerListenerDependencies): (message: unknown, sender: MessageSenderFacts) => void {
  return (message, sender) => {
    if (!dependencies.isTopFrame) return;
    if (sender.id !== dependencies.ownExtensionId || sender.tab !== undefined) return;
    const parsed = parseShowEntryResult(message);
    if (parsed === undefined) return;
    dependencies.banner.show(parsed.report);
  };
}
