// Открытие ушедшей страницы по просьбе плеера `reopen_page` (сессия 11b2, решения 240–241).
//
// Шаги:
// 1. окно: прежнее или любое того же вида (инкогнито или нет); нет — отказ `no_browser_window`;
// 2. вкладка: та же страница уже открыта — берём её, иначе новая вкладка в фоне, без звука, потом активная
//    (окно браузера не всплывает поверх плеера — разведка 11b2, page-reopen-browser.ts);
// 3. владелец сеанса узнаёт, в какой вкладке теперь страница (донесения сторожа оттуда идут этому сеансу);
// 4. взвод сторожа возврата: пока скрипта нет — каждые `PAGE_REOPEN_ARM_RETRY_MS`, потом продление срока;
//    сторож сам перематывает страницу на секунду плеера и ставит тень (next-video-guard.ts);
// 5. тень встала — звук вкладки возвращается как был, итог «вернулась».
// Срок — `PAGE_REOPEN_DEADLINE_MS`. Не успели — отказ с причиной по тому, что видели: скрипта так и не было —
// `page_did_not_load`; на странице видео другой длины — `not_the_same_video`; иначе — `video_did_not_start`.
// Отказ и отмена закрывают вкладку, которую открыли мы; вкладку зрителя оставляют, вернув ей звук.
//
// Итог сообщается ровно один раз (`finished`); отмена (`cancel`) итога не сообщает — сеанс кончается сам.

import { PAGE_RETURN_DURATION_TOLERANCE_S, type PageReturnTarget } from "../../../shared/handoff-contract.ts";
import type { PageReopenFailure } from "../../../protocol/index.ts";
import type { Scheduler } from "../../host-link/native-port.ts";
import { createTimerSlot } from "../../host-link/timer-slot.ts";
import { PAGE_REOPEN_ARM_RETRY_MS, PAGE_REOPEN_DEADLINE_MS } from "../constants.ts";
import { NEXT_VIDEO_ARM_LEASE_MS, NEXT_VIDEO_ARM_REFRESH_MS } from "../video-relocation.ts";
import type { PageReopenBrowser, PageReturnArmOutcome, ReopenedTab, ReturnPageVideo } from "./page-reopen-browser.ts";
import type { ReturnablePage } from "./page-return-memory.ts";

/** Итог открытия. */
export type PageReopenOutcome = { readonly kind: "returned" } | { readonly kind: "failed"; readonly reason: PageReopenFailure };

export interface PageReopenRunDependencies {
  readonly browser: PageReopenBrowser;
  readonly scheduler: Scheduler;
  /** Отметка в журнал разработчика: сбои браузера, которые итог не меняют. */
  readonly note: (message: string) => void;
}

/** Что открыть и что на странице искать. */
export interface PageReopenRequest {
  readonly page: ReturnablePage;
  /**
   * Цель взвода СЕЙЧАС: пока страница открывается, плеер играет скачанное и уходит вперёд, поэтому каждый
   * взвод несёт последнюю секунду часов, а не секунду просьбы (иначе новое поколение отбросило бы плеер назад).
   */
  readonly currentTarget: () => PageReturnTarget;
}

export interface PageReopenRunEvents {
  /** Страница сеанса теперь живёт в этой вкладке (`page` — её описание, `undefined` — браузер не описал). */
  pageIsHere(tabId: number, page: ReturnablePage | undefined): void;
  /** Итог: ровно один раз, отмена его не сообщает. */
  finished(outcome: PageReopenOutcome): void;
}

export interface PageReopenRun {
  /** Тень встала на видео вернувшейся страницы (`handoff_began{started}` из этой вкладки). */
  shadowStood(): void;
  /** Видео узнали, но тень не встала (страница ответила отказом или не ответила): видео не запустилось. */
  shadowDidNotStand(): void;
  /** Браузер сообщил о закрытии вкладки. Наша — страница не загрузилась (зритель её закрыл). */
  tabRemoved(tabId: number): void;
  /** Сеанс кончился (окно плеера закрыто, связь порвалась): открытие отменить, нашу вкладку закрыть. */
  cancel(): void;
}

export function startPageReopen(dependencies: PageReopenRunDependencies, request: PageReopenRequest, events: PageReopenRunEvents): PageReopenRun {
  const { browser, scheduler, note } = dependencies;
  const deadline = createTimerSlot(scheduler);
  const armTimer = createTimerSlot(scheduler);
  let done = false;
  let tab: ReopenedTab | undefined;
  let tabGone = false;
  // Наш скрипт хоть раз ответил из вкладки: страница загрузилась.
  let scriptSeen = false;

  /** Конец работы: таймеры сняты, вкладка — как решено. Повтор — ничего. */
  function settle(leaveTab: "keep_page" | "undo"): boolean {
    if (done) return false;
    done = true;
    deadline.cancel();
    armTimer.cancel();
    if (tab === undefined || tabGone) return true;
    if (leaveTab === "keep_page") restoreSound(tab);
    else undoTab(tab);
    return true;
  }

  function restoreSound(opened: ReopenedTab): void {
    browser.setMuted(opened.tabId, opened.wasMuted).catch((error: unknown) => note(`page return: could not restore tab sound: ${String(error)}`));
  }

  /** Вкладку, открытую нами, закрыть; вкладку зрителя оставить, вернув ей звук. */
  function undoTab(opened: ReopenedTab): void {
    if (opened.openedBy === "viewer") {
      restoreSound(opened);
      return;
    }
    browser.closeTab(opened.tabId).catch((error: unknown) => note(`page return: could not close the reopened tab: ${String(error)}`));
  }

  function fail(reason: PageReopenFailure): void {
    if (settle("undo")) events.finished({ kind: "failed", reason });
  }

  // ---------------------------------------------------------------- шаги

  async function open(): Promise<void> {
    const windowId = await browser.findWindow(request.page).catch(() => undefined);
    if (done) return;
    if (windowId === undefined) {
      fail("no_browser_window");
      return;
    }
    const openTab = await browser.findOpenTab(request.page).catch(() => undefined);
    if (done) return;
    let opened: ReopenedTab;
    try {
      opened = openTab === undefined ? await browser.openTab(windowId, request.page.url) : await browser.bringForwardMuted(openTab);
    } catch (error) {
      note(`page return: could not open the page: ${String(error)}`);
      fail("page_did_not_load");
      return;
    }
    if (done) {
      // Отменили, пока браузер открывал: вкладка не должна остаться.
      undoTab(opened);
      return;
    }
    tab = opened;
    const where = await browser.describeTab(opened.tabId).catch(() => undefined);
    if (done) return;
    events.pageIsHere(opened.tabId, where);
    arm();
  }

  function arm(): void {
    if (done || tab === undefined) return;
    browser.armPageReturn(tab.tabId, NEXT_VIDEO_ARM_LEASE_MS, request.currentTarget()).then(
      (outcome) => scheduleNextArm(outcome),
      () => scheduleNextArm("no_script"),
    );
  }

  /** Скрипт есть — продлевать срок взвода; нет — звать чаще, пока страница грузится. */
  function scheduleNextArm(outcome: PageReturnArmOutcome): void {
    if (done) return;
    if (outcome === "armed") scriptSeen = true;
    armTimer.start(outcome === "armed" ? NEXT_VIDEO_ARM_REFRESH_MS : PAGE_REOPEN_ARM_RETRY_MS, arm);
  }

  /** Срок вышел: причина — по тому, что видели (см. шапку). */
  async function failByDeadline(): Promise<void> {
    if (done) return;
    if (!scriptSeen || tab === undefined) {
      fail("page_did_not_load");
      return;
    }
    const videos = await browser.readVideos(tab.tabId).catch(() => [] as readonly ReturnPageVideo[]);
    fail(onlyOtherVideos(videos, request.currentTarget()) ? "not_the_same_video" : "video_did_not_start");
  }

  deadline.start(PAGE_REOPEN_DEADLINE_MS, () => void failByDeadline());
  open().catch((error: unknown) => {
    note(`page return: opening failed: ${String(error)}`);
    fail("page_did_not_load");
  });

  return {
    shadowStood() {
      if (settle("keep_page")) events.finished({ kind: "returned" });
    },
    shadowDidNotStand: () => fail("video_did_not_start"),
    tabRemoved(tabId) {
      if (tab?.tabId !== tabId) return;
      tabGone = true;
      fail("page_did_not_load");
    },
    cancel: () => void settle("undo"),
  };
}

/** На странице есть видео, и ни одно не похоже на искомое: страница показывает другое (одностраничник). */
function onlyOtherVideos(videos: readonly ReturnPageVideo[], target: PageReturnTarget): boolean {
  if (videos.length === 0) return false;
  return !videos.some((video) => videoMatchesReturnTarget(video, target));
}

/** Видео — то, что в плеере: запись той же длины (± допуск) или эфир. Фон сверяет и донесение сторожа: страница — чужой код. */
export function videoMatchesReturnTarget(video: ReturnPageVideo, target: PageReturnTarget): boolean {
  if (target.kind === "live") return video.durationS === null;
  return video.durationS !== null && Math.abs(video.durationS - target.duration_s) <= PAGE_RETURN_DURATION_TOLERANCE_S;
}
