// Модель вкладки (сессия 09c, решение 198): снимок «какие видео нашли на вкладке» после последнего
// опроса фреймов, плюс причина последнего отказа передачи.
//
// Владелец состояния — этот модуль. Читают попап (сессия 10: `videosFoundIn`, `mainVideoOf`,
// `lastRefusalOf`) и читатель рекламы (09d: `isAdOn`) ТОЛЬКО этими методами. Модель не знает о
// сеансах передачи и ничего в них не меняет: методы пишут и читают лишь снимки.
//
// Модель — кэш. Снимок лежит в памяти и в `storage.session` (фон Chrome засыпает: новый объект
// модели читает снимок из хранилища). Сбой хранилища передачу не ломает: выбор идёт по свежему
// опросу, сбой попадает в журнал один раз за жизнь модели, а чтение при сбое — «снимка нет».
// Но сбой чтения — не «снимка нет» для ЗАПИСИ: отказ лёг бы поверх снимка, которого мы не видели.
//
// Запись живёт только у открытых вкладок:
// - забытая вкладка не воскресает: запоздавшие запись опроса и отказ приходят после `forgetTab`;
// - на старте фона `dropClosedTabs` убирает записи вкладок, закрытых, пока фон спал;
// - `tabs.onReplaced` (предзагрузка страницы подменила вкладку) переносит запись на новый номер.

import type { FrameCandidates } from "../handoff/video-choice.ts";
import { buildSnapshot, emptySnapshot, parseSnapshot, refOf, sameRef, type TabRefusal, type TabSnapshot, type TabVideoRecord, type VideoRef } from "./tab-snapshot.ts";
import type { TabModelStore } from "./session-store.ts";

export type { TabModelStore } from "./session-store.ts";
export type { TabRefusal, TabSnapshot, TabVideoRecord, VideoRef } from "./tab-snapshot.ts";

/** Префикс ключа в хранилище; один ключ на вкладку. */
const STORE_KEY_PREFIX = "tabModel:";

/** Intent-методы модели вкладки. Ни один не бросает: сбой хранилища гасится внутри. */
export interface TabModel {
  /** Записать итог свежего опроса фреймов; прежний отказ вкладки сбрасывается. */
  recordSurvey(tabId: number, frames: readonly FrameCandidates[]): Promise<void>;
  /** Видео, найденные на вкладке при последнем опросе (хвост ≤ 64, самые вероятные первыми). */
  videosFoundIn(tabId: number): Promise<readonly TabVideoRecord[]>;
  /** Главное видео по правилу выбора; `null` — не опрашивали или передавать нечего. */
  mainVideoOf(tabId: number): Promise<VideoRef | null>;
  /** Идёт ли на этом элементе отдельный рекламный ролик: вердикт `ad` и играет при последнем опросе. */
  isAdOn(tabId: number, video: VideoRef): Promise<boolean>;
  /** Запомнить причину отказа последней передачи (закрытый список). */
  noteHandoffRefused(tabId: number, reason: TabRefusal): Promise<void>;
  /** Причина последнего отказа; `null` — отказа не было. */
  lastRefusalOf(tabId: number): Promise<TabRefusal | null>;
  /** Вкладка закрыта: стереть снимок; запоздавшие записи для неё дальше ничего не делают. */
  forgetTab(tabId: number): Promise<void>;
  /** Браузер заменил вкладку `fromTabId` на `toTabId`: снимок переезжает на новый номер. */
  moveTab(fromTabId: number, toTabId: number): Promise<void>;
  /** Старт фона: стереть записи вкладок, которых уже нет (сбой чтения ничего не стирает). */
  dropClosedTabs(openTabIds: readonly number[]): Promise<void>;
}

export interface TabModelDependencies {
  readonly store: TabModelStore;
  /** Часы фона, мс. */
  readonly now: () => number;
  /** Журнал сбоя хранилища; зовётся не чаще раза за жизнь модели. */
  readonly reportStoreFailure: (error: unknown) => void;
}

/** Чтение снимка: `failed` отличается от `absent`, поверх несчитанного снимка писать нельзя. */
type SnapshotRead = { readonly kind: "found"; readonly snapshot: TabSnapshot } | { readonly kind: "absent" } | { readonly kind: "failed" };

/** Номер вкладки из ключа хранилища; `undefined` — ключ не наш. */
function tabIdOfKey(key: string): number | undefined {
  if (!key.startsWith(STORE_KEY_PREFIX)) return undefined;
  const tabId = Number(key.slice(STORE_KEY_PREFIX.length));
  return Number.isSafeInteger(tabId) ? tabId : undefined;
}

export function createTabModel(dependencies: TabModelDependencies): TabModel {
  const cache = new Map<number, TabSnapshot>();
  // Закрытые за эту жизнь фона. Номера вкладок браузер не использует повторно, поэтому пометка вечная.
  const forgotten = new Set<number>();
  let storeFailureReported = false;

  function reportFailure(error: unknown): void {
    if (storeFailureReported) return;
    storeFailureReported = true;
    dependencies.reportStoreFailure(error);
  }

  /** Выполнить операцию хранилища; сбой не пробрасывается, а попадает в журнал один раз. */
  async function guarded<Result>(operation: () => Promise<Result>, fallback: Result): Promise<Result> {
    try {
      return await operation();
    } catch (error) {
      reportFailure(error);
      return fallback;
    }
  }

  const keyOf = (tabId: number): string => `${STORE_KEY_PREFIX}${tabId}`;

  async function readSnapshot(tabId: number): Promise<SnapshotRead> {
    const cached = cache.get(tabId);
    if (cached !== undefined) return { kind: "found", snapshot: cached };
    let stored: unknown;
    try {
      stored = await dependencies.store.get(keyOf(tabId));
    } catch (error) {
      reportFailure(error);
      return { kind: "failed" };
    }
    const parsed = parseSnapshot(stored, tabId);
    // Пока ждали хранилище, мог прийти свежий опрос (он новее прочитанного) или `forgetTab`.
    if (parsed !== undefined && !cache.has(tabId) && !forgotten.has(tabId)) cache.set(tabId, parsed);
    const current = cache.get(tabId);
    return current === undefined ? { kind: "absent" } : { kind: "found", snapshot: current };
  }

  async function snapshotOf(tabId: number): Promise<TabSnapshot | undefined> {
    const read = await readSnapshot(tabId);
    return read.kind === "found" ? read.snapshot : undefined;
  }

  async function save(snapshot: TabSnapshot): Promise<void> {
    if (forgotten.has(snapshot.tab_id)) return;
    cache.set(snapshot.tab_id, snapshot);
    await guarded(() => dependencies.store.set(keyOf(snapshot.tab_id), snapshot), undefined);
  }

  async function erase(tabId: number): Promise<void> {
    cache.delete(tabId);
    await guarded(() => dependencies.store.remove(keyOf(tabId)), undefined);
  }

  return {
    recordSurvey: (tabId, frames) => save(buildSnapshot(tabId, frames, dependencies.now())),
    async videosFoundIn(tabId) {
      return (await snapshotOf(tabId))?.videos ?? [];
    },
    async mainVideoOf(tabId) {
      return (await snapshotOf(tabId))?.main ?? null;
    },
    async isAdOn(tabId, video) {
      const snapshot = await snapshotOf(tabId);
      return snapshot?.videos.some((record) => record.verdict === "ad" && record.playing && sameRef(refOf(record), video)) ?? false;
    },
    async noteHandoffRefused(tabId, reason) {
      const read = await readSnapshot(tabId);
      // Снимок не прочитан: отказ теряется, зато лежащий снимок цел. Сбой уже в журнале.
      if (read.kind === "failed") return;
      await save({ ...(read.kind === "found" ? read.snapshot : emptySnapshot(tabId)), last_refusal: reason });
    },
    async lastRefusalOf(tabId) {
      return (await snapshotOf(tabId))?.last_refusal ?? null;
    },
    async forgetTab(tabId) {
      forgotten.add(tabId);
      await erase(tabId);
    },
    async moveTab(fromTabId, toTabId) {
      const read = await readSnapshot(fromTabId);
      // Старой вкладки больше нет: запись под старым номером не должна появиться снова.
      forgotten.add(fromTabId);
      // Не прочитали: старая запись остаётся, её уберёт уборка на следующем старте фона.
      if (read.kind === "failed") return;
      // Опрос новой вкладки новее переезжающего снимка.
      if (read.kind === "found" && !cache.has(toTabId)) await save({ ...read.snapshot, tab_id: toTabId });
      await erase(fromTabId);
    },
    async dropClosedTabs(openTabIds) {
      const open = new Set(openTabIds);
      const keys = await guarded(() => dependencies.store.keys(), []);
      for (const key of keys) {
        const tabId = tabIdOfKey(key);
        // Снимок в кэше появился уже в этой жизни: вкладка открылась после списка `openTabIds`.
        if (tabId !== undefined && !open.has(tabId) && !cache.has(tabId)) await guarded(() => dependencies.store.remove(key), undefined);
      }
    },
  };
}
