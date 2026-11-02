// Шов MAIN-мира с «тенью» (пакет A2, `./shadow/index.ts`) — как его видит контроллер передачи.
//
// Контроллеру не нужно знать окружение тени (оригиналы свойств элемента, гашение mediaSession,
// таймеры): это собирает `main.ts` один раз на document_start и отдаёт сюда готовую функцию.
// Так контроллер тестируется с фейковой тенью, а настоящая подключается в одном месте.

import type { HandoffEndPosition, HandoffEndResume, PageEvent, ShadowCommand } from "../shared/handoff-contract.ts";

/** Запущенная тень одного элемента (подмножество `ShadowSession` из `./shadow/index.ts`). */
export interface ShadowSessionHandle {
  apply(command: ShadowCommand): void;
  /** Снять тень; тень шлёт `handoff_ended{returned}` через свой `emit`. */
  end(position: HandoffEndPosition, resume: HandoffEndResume): void;
}

/** Точный ответ реестра MAIN-мира: адрес создан `URL.createObjectURL` для MediaSource документа. */
export type MediaSourceObjectUrlCheck = (url: string) => boolean;

/**
 * Поставить элемент в тень. `emit` — события страницы (page_seek, page_playback, handoff_ended, …).
 * `isMediaSourceObjectUrl` — знание реестра (тень по нему отличает новое поколение на том же
 * элементе от blob-адреса обычного файла).
 */
export type ShadowStarter = (
  element: HTMLMediaElement,
  emit: (event: PageEvent) => void,
  isMediaSourceObjectUrl: MediaSourceObjectUrlCheck,
) => ShadowSessionHandle;
