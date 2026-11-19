// Возврат «Назад» в ленту после ухода из неё (сессия 16b3b, решения 375–376).
//
// Уход из ленты в другой раздел кончает сеанс: окно стоит на паузе с «Source tab closed» (решение 370), сеанса и
// его записи во владельце сеансов (`tab-handoffs.ts`) больше нет. Человек жмёт «Назад» в браузере, вкладка снова в
// ленте, страница снова играет ролик (TikTok возвращает прежний элемент и играет его с начала, наблюдение 16b3).
// Решение владельца 375: этот ролик уходит в ТО ЖЕ окно, как при перезагрузке страницы (16c). Процесс окна ещё жив
// и ждёт зрителя, поэтому новый обычный сеанс встаёт в нём же (плеер: «новый сеанс во время доигрывания заменяет
// доигрываемый»).
//
// Модуль владеет короткой записью «откуда вкладка ушла из ленты» — по одной на вкладку:
// - раздел ленты (`feed-section.ts`), из которого ушли;
// - срок записи: как ожидание перезагрузки страницы (настройка «Page reload», решение 376). Позже «Назад» — ролик
//   играет в браузере, в окно — кнопкой;
// - взвод сторожа следующего видео, ТОЛЬКО пока вкладка снова в этом разделе: в другом разделе играет своё тихое
//   превью, и окно не должно его брать (16b3). Взвод обходит видео фрейма и ловит уже играющее, так что ролик,
//   запущенный страницей чуть раньше новости об адресе, тоже будет задержан;
// - ответ на задержанное видео: вкладка в разделе ленты — `adopt_new_session` (тень ждёт `begin_handoff` нового
//   сеанса) и обычная передача вкладки с этим видео; не состоялась — видео отпускается во фрейме.
// Новый сеанс вкладки (кнопка, автозапуск, этот же подхват), закрытие вкладки или срок — запись забыта.
//
// Сеансами и их выбором видео владеет `tab-handoffs.ts`: сюда приходят только факты, а передача уходит через порт.
// Время — монотонные часы фона, таймеры — `Scheduler`: таймеры скрытой вкладки спят.

import type { HeldVideoReply, HeldVideoReport, VideoRef } from "../../../shared/handoff-contract.ts";
import type { Scheduler } from "../../host-link/native-port.ts";
import { createTimerSlot, type TimerSlot } from "../../host-link/timer-slot.ts";
import type { WatchTabUpdates } from "../episode-change/session-episode-change.ts";
import { NEXT_VIDEO_ARM_LEASE_MS, NEXT_VIDEO_ARM_REFRESH_MS } from "../video-relocation.ts";
import { feedSectionOf, type FeedSection } from "./feed-section.ts";

/** Чем кончилась передача вернувшегося ролика: встал в окне или нет (причину пишет владелец сеансов). */
export type ReturnedVideoHandoff = "started" | "not_started";

export interface FeedReturnWatchPorts {
  readonly scheduler: Scheduler;
  readonly watchTabUpdates: WatchTabUpdates;
  /** Взвести сторож следующего видео во всех фреймах вкладки на `leaseMs`. */
  readonly armNextVideo: (tabId: number, leaseMs: number) => void;
  /** Передать вкладку с этим видео (обычная передача: опрос, выбор, проверка адреса, новый сеанс). */
  readonly handOffReturnedVideo: (tabId: number, video: VideoRef) => Promise<ReturnedVideoHandoff>;
  /** Отпустить задержанное видео во фрейме: страница играет его сама. */
  readonly releaseHeldVideo: (tabId: number, frameId: number, element: number) => void;
  readonly noteForDeveloper: (message: string) => void;
}

export interface FeedReturnWatch {
  /**
   * Сеанс вкладки кончился уходом из ленты раздела `section`; запись живёт `waitMs` (настройка «Page reload»).
   * `0` — не ждать вовсе (человек выключил ожидание перезагрузки).
   */
  feedLeft(tabId: number, section: FeedSection, waitMs: number): void;
  /**
   * Сторож задержал видео во вкладке без сеанса. `undefined` — записи нет, решать не этому модулю; иначе ответ
   * странице.
   */
  heldVideo(tabId: number, frameId: number, report: HeldVideoReport): HeldVideoReply | undefined;
  /** У вкладки начался новый сеанс или вкладку закрыли: запись больше не нужна. Повтор безвреден. */
  forgetTab(tabId: number): void;
  /** Есть ли у вкладки запись «ушли из ленты» (для попапа и тестов). */
  remembersFeedOf(tabId: number): boolean;
}

/** Запись вкладки. */
interface FeedLeft {
  readonly section: FeedSection;
  /** Срок записи. */
  readonly deadline: TimerSlot;
  /** Продление взвода сторожа, пока вкладка в разделе ленты. */
  readonly arming: TimerSlot;
  /** Отписка от новостей об адресе вкладки. */
  readonly unsubscribe: () => void;
  /** Вкладка сейчас в разделе ленты (по последней новости об адресе). */
  inFeed: boolean;
  /** Видео уже отдано передаче: второе не берём, пока она не кончится. */
  handingOver: boolean;
}

export function createFeedReturnWatch(ports: FeedReturnWatchPorts): FeedReturnWatch {
  const records = new Map<number, FeedLeft>();

  function forgetTab(tabId: number): void {
    const record = records.get(tabId);
    if (record === undefined) return;
    records.delete(tabId);
    record.deadline.cancel();
    record.arming.cancel();
    record.unsubscribe();
  }

  /** Взвод сторожа с продлением — как у переезда сеанса (`video-relocation.ts`), пока вкладка в ленте. */
  function renewArming(tabId: number, record: FeedLeft): void {
    ports.armNextVideo(tabId, NEXT_VIDEO_ARM_LEASE_MS);
    record.arming.start(NEXT_VIDEO_ARM_REFRESH_MS, () => renewArming(tabId, record));
  }

  /** Новость об адресе вкладки: вернулась в раздел ленты — взвести сторож, ушла снова — снять продление. */
  function addressSeen(tabId: number, record: FeedLeft, url: string): void {
    record.inFeed = feedSectionOf(url) === record.section;
    if (!record.inFeed) {
      // Взвод сам истечёт через `NEXT_VIDEO_ARM_LEASE_MS`: команды «снять» у сторожа нет.
      record.arming.cancel();
      return;
    }
    if (!record.arming.isActive()) renewArming(tabId, record);
  }

  /** Передача задержанного ролика; не встала — отпустить его, запись ждёт дальше до своего срока. */
  function handOver(tabId: number, record: FeedLeft, frameId: number, video: VideoRef): void {
    record.handingOver = true;
    const notStarted = (): void => {
      ports.releaseHeldVideo(tabId, frameId, video.element);
      if (records.get(tabId) === record) record.handingOver = false;
    };
    ports.handOffReturnedVideo(tabId, video).then(
      (outcome) => {
        // Встал: новый сеанс сам забудет запись (`forgetTab` по старту сеанса); здесь — на случай гонки.
        if (outcome === "started") forgetTab(tabId);
        else notStarted();
      },
      (error: unknown) => {
        ports.noteForDeveloper(`feed return: the handoff of the returned video threw: ${String(error)}`);
        notStarted();
      },
    );
  }

  return {
    feedLeft(tabId, section, waitMs) {
      forgetTab(tabId);
      if (waitMs <= 0) return;
      const record: FeedLeft = {
        section,
        deadline: createTimerSlot(ports.scheduler),
        arming: createTimerSlot(ports.scheduler),
        unsubscribe: ports.watchTabUpdates((update) => {
          if (update.tabId === tabId && update.url !== undefined) addressSeen(tabId, record, update.url);
        }),
        // Уход доказан сменой раздела: в миг конца вкладка не в ленте.
        inFeed: false,
        handingOver: false,
      };
      records.set(tabId, record);
      record.deadline.start(waitMs, () => forgetTab(tabId));
    },
    heldVideo(tabId, frameId, report) {
      const record = records.get(tabId);
      if (record === undefined) return undefined;
      // Превью другого раздела и второе видео во время передачи окно не уводят.
      if (!record.inFeed || record.handingOver) return "release";
      const candidate = report.candidates.find((video) => video.element === report.element);
      if (candidate === undefined) return "release";
      handOver(tabId, record, frameId, { frame_id: frameId, document: candidate.document, element: candidate.element });
      return "adopt_new_session";
    },
    forgetTab,
    remembersFeedOf: (tabId) => records.has(tabId),
  };
}
