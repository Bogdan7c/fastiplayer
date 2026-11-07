// Стойка номерков (сессия 10a2, решение 228): фон выдаёт плееру номерок вместо настоящего адреса,
// а по номерку из `fetch_resource` находит адрес. Плеер настоящих адресов не видит (приватность),
// а фон качает ТОЛЬКО то, что выдал сам: страница не заставит его скачать чужое через номерок.
//
// Одна стойка на сеанс передачи. Инварианты:
// - номерки растут через ВСЕ поколения сеанса (счётчик не сбрасывается): запоздалый запрос
//   прежнего поколения получит `refused`, а не чужой ресурс нового;
// - `beginGeneration` забывает все записи, счётчик сохраняет;
// - один адрес в поколении - один номерок (перечитывание живого плейлиста таблицу не раздувает);
// - ссылка «плейлист» сильнее ссылки «медиа»: плейлист качается без кэша, это безопаснее;
// - таблица ограничена: старейшие записи вытесняются первыми, вытесненный номерок - `refused`;
// - номерок REFUSED_RESOURCE_HANDLE не выдаётся; счётчик останавливается перед ним.

import type { ResourceHandle } from "../../../protocol/index.ts";
import { FIRST_RESOURCE_HANDLE, MAX_HANDLE_DESK_ENTRIES, REFUSED_RESOURCE_HANDLE } from "./limits.ts";

/** Вид ресурса: от него зависит кэш браузера и то, как ответ разбирается. */
export type ResourceKind =
  /** Плейлист: качается без кэша (живые плейлисты меняются), тело переписывается. */
  | "playlist"
  /** Сегмент, init, ключ, часть LL-HLS: кэш браузера помогает перемотке назад. */
  | "media";

/** Что стойка помнит про номерок. */
export interface DeskEntry {
  readonly url: string;
  readonly kind: ResourceKind;
}

/** Пределы стойки (в продукте - значения по умолчанию, тесты сужают). */
export interface HandleDeskLimits {
  /** Сколько записей держать, прежде чем вытеснять старейшие. */
  readonly maxEntries: number;
  /** С какого номерка начинать (тест исчерпания ставит число около потолка). */
  readonly firstHandle: ResourceHandle;
}

export const DEFAULT_HANDLE_DESK_LIMITS: HandleDeskLimits = {
  maxEntries: MAX_HANDLE_DESK_ENTRIES,
  firstHandle: FIRST_RESOURCE_HANDLE,
};

export interface HandleDesk {
  /** Новое поколение: все прежние записи забыты, счётчик продолжается. */
  beginGeneration(): void;
  /** Номерок для адреса; `undefined` - номерки кончились (практически недостижимо). */
  issue(url: string, kind: ResourceKind): ResourceHandle | undefined;
  /** Что стоит за номерком; `undefined` - неизвестен, вытеснен, из прошлого поколения или «отказ». */
  lookup(handle: ResourceHandle): DeskEntry | undefined;
  /** Сколько записей сейчас в стойке (для диагностики и тестов). */
  size(): number;
}

interface MutableDeskEntry {
  readonly url: string;
  kind: ResourceKind;
}

export function createHandleDesk(limits: HandleDeskLimits = DEFAULT_HANDLE_DESK_LIMITS): HandleDesk {
  // Map помнит порядок вставки: первый ключ - самая старая запись (очередь вытеснения FIFO).
  const entryByHandle = new Map<ResourceHandle, MutableDeskEntry>();
  const handleByUrl = new Map<string, ResourceHandle>();
  let nextHandle = limits.firstHandle;

  /** Выбросить самую старую запись. */
  function evictOldest(): void {
    const oldest = entryByHandle.keys().next();
    if (oldest.done === true) return;
    const entry = entryByHandle.get(oldest.value);
    entryByHandle.delete(oldest.value);
    if (entry !== undefined) handleByUrl.delete(entry.url);
  }

  return {
    beginGeneration() {
      entryByHandle.clear();
      handleByUrl.clear();
      // Счётчик НЕ трогаем: номерки прошлого поколения не должны совпасть с новыми.
    },
    issue(url, kind) {
      const existing = handleByUrl.get(url);
      if (existing !== undefined) {
        const entry = entryByHandle.get(existing);
        // Плейлист сильнее медиа: вид только повышается.
        if (entry !== undefined && kind === "playlist") entry.kind = "playlist";
        return existing;
      }
      // Номерок «отказ» зарезервирован: дойдя до него, выдавать больше нечего.
      if (nextHandle >= REFUSED_RESOURCE_HANDLE) return undefined;
      while (entryByHandle.size >= limits.maxEntries) evictOldest();
      const handle = nextHandle;
      nextHandle += 1;
      entryByHandle.set(handle, { url, kind });
      handleByUrl.set(url, handle);
      return handle;
    },
    lookup(handle) {
      const entry = entryByHandle.get(handle);
      return entry === undefined ? undefined : { url: entry.url, kind: entry.kind };
    },
    size: () => entryByHandle.size,
  };
}
