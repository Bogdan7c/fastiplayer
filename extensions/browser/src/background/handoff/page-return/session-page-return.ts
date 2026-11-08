// Возврат страницы в сеансе передачи (сессия 11b2, решения 240–241): владелец стадии «страница на месте →
// сирота → открываем → вернулась или отказ» и памяти возврата. Сеанс (`handoff-session.ts`, у предела
// размера) только сообщает сюда события и выполняет ответы.
//
// Договор с плеером (11b1):
// - уход страницы, которую можно вернуть (page-return-rules.ts), — `page_left{reason}`, связь остаётся;
// - пока страницы нет, плеер шлёт часы — по ним память знает последнюю секунду;
// - `reopen_page` — один раз на сироту: повторная просьба ничего не начинает;
// - тень встала в новой вкладке — `page_returned`, ДО первого события нового документа (поколение N+1);
// - не вышло — `page_reopen_failed{reason}`, закрытие 4002 и конец сеанса.

import type { ProtocolVersion, TabGoneReason } from "../../../protocol/index.ts";
import type { HandoffSessionEnd } from "../session-contract.ts";
import type { SessionSocket } from "../session-socket.ts";
import type { MediaTimeMicros } from "../../../protocol/time.ts";
import type { Scheduler } from "../../host-link/native-port.ts";
import type { PageReopenBrowser } from "./page-reopen-browser.ts";
import { startPageReopen, videoMatchesReturnTarget, type PageReopenRun } from "./page-reopen-run.ts";
import { createPageReturnMemory, type ReturnablePage, type VideoInPlayer } from "./page-return-memory.ts";
import { decidePageLeave } from "./page-return-rules.ts";
import type { PageReturnTarget } from "../../../shared/handoff-contract.ts";

/** Что фасаду нужно от сеанса и браузера. */
export interface SessionPageReturnPorts {
  readonly browser: PageReopenBrowser;
  readonly scheduler: Scheduler;
  readonly note: (message: string) => void;
  /** Связь сеанса с плеером (`undefined` — ещё или уже нет). */
  readonly socket: () => SessionSocket | undefined;
  /** Сирота: сторона страницы затихает (тени, порта, загрузок нет) — ДО `page_left`, иначе байт мог бы уйти после. */
  readonly quietenPageSide: () => void;
  /** Страница сеанса теперь живёт в этой вкладке (`url` — её адрес): туда команды, оттуда донесения сторожа. */
  readonly pageMovedToTab: (tabId: number, url: string | undefined) => void;
  /** Сеанс кончается (здесь — отказом возврата). */
  readonly endSession: (end: HandoffSessionEnd) => void;
}

/** Чем кончился уход страницы. */
export type PageLeftOutcome =
  /** Плееру ушёл `page_left`, связь держим: ждём просьбы зрителя. */
  | "orphaned"
  /** Вернуть нельзя: сеанс кончается прежним `tab_gone`. */
  | "not_returnable";

export interface SessionPageReturn {
  /** Страница живёт здесь (при старте сеанса и после возврата). */
  notePage(page: ReturnablePage | undefined): void;
  /** Тень встала на видео: что теперь в плеере. */
  noteVideoInPlayer(video: VideoInPlayer): void;
  /** Тик часов плеера. */
  notePlayerSecond(positionUs: MediaTimeMicros): void;
  /** Страница ушла. */
  pageLeft(reason: TabGoneReason, agreedProtocol: ProtocolVersion): PageLeftOutcome;
  /** Плеер просит вернуть страницу (`reopen_page`): начать открытие, если сирота ещё не просила. */
  reopenRequested(): void;
  /** Идёт открытие страницы (просьба принята, итога ещё нет). */
  isReturning(): boolean;
  /** Идёт открытие, и задержанное видео той же длины (или эфир): брать его. */
  acceptsReturnedVideo(durationS: number | null): boolean;
  /** Тень встала в новой вкладке: `page_returned`, страница снова на месте. */
  shadowStood(): void;
  /** Видео взяли, а тень не встала: отказ «видео не запустилось». */
  shadowDidNotStand(): void;
  /** Браузер закрыл вкладку (`tabs.onRemoved`). */
  tabRemoved(tabId: number): void;
  /** Сеанс кончается: открытие отменить, нашу вкладку закрыть. */
  cancel(): void;
}

/** Стадия возврата. */
type ReturnStage =
  | { readonly kind: "page_present" }
  | { readonly kind: "orphan"; readonly page: ReturnablePage; readonly target: PageReturnTarget }
  | { readonly kind: "returning"; readonly run: PageReopenRun; readonly target: PageReturnTarget }
  | { readonly kind: "over" };

export function createSessionPageReturn(ports: SessionPageReturnPorts): SessionPageReturn {
  const memory = createPageReturnMemory();
  let stage: ReturnStage = { kind: "page_present" };

  function startReturn(page: ReturnablePage, target: PageReturnTarget): void {
    const run = startPageReopen(
      { browser: ports.browser, scheduler: ports.scheduler, note: ports.note },
      // Секунда — последняя из часов на момент взвода (договор 11b1: плеер всё это время играет скачанное).
      { page, currentTarget: () => memory.returnTarget() ?? target },
      {
        pageIsHere: (tabId, where) => {
          // Новая вкладка — новое «где жила страница»: следующий уход вернёт уже её.
          memory.notePage(where);
          ports.pageMovedToTab(tabId, where?.url);
        },
        finished: (outcome) => {
          if (outcome.kind === "returned") return;
          stage = { kind: "over" };
          // Порядок договора 11b1: причина, затем закрытие 4002 — плеер покажет её и досмотрит скачанное.
          ports.socket()?.sendPageReopenFailedAndClose(outcome.reason);
          ports.endSession({ kind: "page_not_returned", reason: outcome.reason });
        },
      },
    );
    stage = { kind: "returning", run, target };
  }

  return {
    notePage: (page) => memory.notePage(page),
    noteVideoInPlayer: (video) => memory.noteVideoInPlayer(video),
    notePlayerSecond: (positionUs) => memory.notePlayerSecond(positionUs),
    pageLeft(reason, agreedProtocol) {
      if (stage.kind !== "page_present") return "not_returnable";
      const decision = decidePageLeave({ reason, agreedProtocol, page: memory.page(), target: memory.returnTarget() });
      if (decision.kind === "tab_gone") return "not_returnable";
      stage = { kind: "orphan", page: decision.page, target: decision.target };
      ports.quietenPageSide();
      ports.socket()?.sendPageLeft(reason);
      return "orphaned";
    },
    reopenRequested() {
      if (stage.kind !== "orphan") {
        ports.note(`reopen_page ignored in stage ${stage.kind}`);
        return;
      }
      startReturn(stage.page, memory.returnTarget() ?? stage.target);
    },
    isReturning: () => stage.kind === "returning",
    acceptsReturnedVideo(durationS) {
      return stage.kind === "returning" && videoMatchesReturnTarget({ durationS }, stage.target);
    },
    shadowStood() {
      if (stage.kind !== "returning") return;
      const { run } = stage;
      stage = { kind: "page_present" };
      // В очередь сокета ДО событий нового документа: за ним плеер ждёт поколение N+1 (договор 11b1).
      ports.socket()?.enqueue({ kind: "message", message: { type: "page_returned" } });
      run.shadowStood();
    },
    shadowDidNotStand() {
      if (stage.kind === "returning") stage.run.shadowDidNotStand();
    },
    tabRemoved(tabId) {
      if (stage.kind === "returning") stage.run.tabRemoved(tabId);
    },
    cancel() {
      if (stage.kind === "returning") stage.run.cancel();
      stage = { kind: "over" };
    },
  };
}
