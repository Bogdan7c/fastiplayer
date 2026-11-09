// Хранилище памяти выбора (сессия 14b2, решения 44, 281): единственный владелец записи `rememberedChoices`.
//
// Почему один владелец и очередь: запись памяти — «прочитать документ, поменять, записать». Два таких шага
// вперемешку (два сеанса во вкладках, «Forget» со страницы настроек) затёрли бы друг друга. Поэтому все
// действия встают в одну очередь фона и выполняются по одному, каждое читает хранилище заново. Страница
// настроек сама в память не пишет: она просит фон (`choice-memory-commands.ts`).
//
// Объём — настройка `choiceMemorySettings` (по умолчанию 500): при записи сверх объёма вытесняются записи,
// которых дольше всего не касались (`used_at`). Уменьшенный объём вступает в силу со следующей записи.

import {
  EMPTY_REMEMBERED_CHOICES,
  parseRememberedChoicesDocument,
  REMEMBERED_CHOICES_KEY,
  REMEMBERED_CHOICES_VERSION,
  type RememberedChoicePatch,
  type RememberedChoices,
  type RememberedChoicesDocument,
  type SeriesFingerprint,
} from "../../shared/choice-memory-contract.ts";
import { readChoiceMemorySettings } from "../../shared/choice-memory-settings.ts";
import type { KeyValueStore } from "../../shared/codec-storage.ts";

export interface ChoiceMemoryStore {
  /** Запись сериала; нет — `undefined`. Время касания не меняет (это делает `touch`). */
  recall(fingerprint: SeriesFingerprint): Promise<RememberedChoices | undefined>;
  /** Заменить поля выбора записи (нет записи — создать), отметить касание и вытеснить лишнее. */
  remember(fingerprint: SeriesFingerprint, patch: RememberedChoicePatch): Promise<void>;
  /** Отметить, что запись применили: её не вытеснят раньше тех, что не нужны давно. Нет записи — ничего. */
  touch(fingerprint: SeriesFingerprint): Promise<void>;
  /** Стереть всю память («Forget remembered choices»). */
  forget(): Promise<void>;
  /** Сколько сериалов сейчас помнит память. */
  count(): Promise<number>;
}

export interface ChoiceMemoryStoreDependencies {
  readonly store: KeyValueStore;
  /** Часы для `used_at` (мс от эпохи). */
  readonly now: () => number;
}

export function createChoiceMemoryStore(dependencies: ChoiceMemoryStoreDependencies): ChoiceMemoryStore {
  const { store, now } = dependencies;
  // Хвост очереди. Сбой одного действия достаётся его вызывающему, а очередь идёт дальше.
  let tail: Promise<unknown> = Promise.resolve();

  function enqueue<T>(action: () => Promise<T>): Promise<T> {
    const result = tail.then(action);
    tail = result.catch(() => undefined);
    return result;
  }

  async function readDocument(): Promise<RememberedChoicesDocument> {
    return parseRememberedChoicesDocument((await store.get([REMEMBERED_CHOICES_KEY]))[REMEMBERED_CHOICES_KEY]);
  }

  async function writeDocument(series: Readonly<Record<SeriesFingerprint, RememberedChoices>>): Promise<void> {
    const document: RememberedChoicesDocument = { version: REMEMBERED_CHOICES_VERSION, series };
    await store.set({ [REMEMBERED_CHOICES_KEY]: document });
  }

  return {
    recall: (fingerprint) => enqueue(async () => (await readDocument()).series[fingerprint]),
    remember: (fingerprint, patch) =>
      enqueue(async () => {
        const [document, settings] = await Promise.all([readDocument(), readChoiceMemorySettings(store)]);
        const updated: RememberedChoices = { ...document.series[fingerprint], ...patch, used_at: now() };
        await writeDocument(withinCapacity({ ...document.series, [fingerprint]: updated }, settings.capacity, fingerprint));
      }),
    touch: (fingerprint) =>
      enqueue(async () => {
        const document = await readDocument();
        const existing = document.series[fingerprint];
        if (existing === undefined) return;
        await writeDocument({ ...document.series, [fingerprint]: { ...existing, used_at: now() } });
      }),
    forget: () => enqueue(() => store.set({ [REMEMBERED_CHOICES_KEY]: EMPTY_REMEMBERED_CHOICES })),
    count: () => enqueue(async () => Object.keys((await readDocument()).series).length),
  };
}

/**
 * Оставить не больше `capacity` записей: уходят те, которых дольше всего не касались. Только что записанная
 * (`keep`) не уходит никогда — даже если часы фона отстали и её время оказалось не самым новым.
 */
export function withinCapacity(
  series: Readonly<Record<SeriesFingerprint, RememberedChoices>>,
  capacity: number,
  keep: SeriesFingerprint,
): Readonly<Record<SeriesFingerprint, RememberedChoices>> {
  const fingerprints = Object.keys(series);
  if (fingerprints.length <= capacity) return series;
  const others = fingerprints.filter((fingerprint) => fingerprint !== keep);
  // Самые свежие — в начало; при равном времени порядок по отпечатку, чтобы итог не зависел от порядка ключей.
  others.sort((left, right) => (series[right]?.used_at ?? 0) - (series[left]?.used_at ?? 0) || left.localeCompare(right));
  const kept = [keep, ...others.slice(0, Math.max(0, capacity - 1))];
  return Object.fromEntries(kept.flatMap((fingerprint) => (series[fingerprint] === undefined ? [] : [[fingerprint, series[fingerprint]]])));
}
