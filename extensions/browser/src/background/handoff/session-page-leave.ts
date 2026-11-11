// Уход страницы сеанса передачи: порт к странице умер, вкладку закрыли или ушли со страницы.
//
// Вынесено из `handoff-session.ts` без изменения поведения (сессия 16a: сеанс упёрся в предел
// 800 строк, а 16a добавляет новые правила концов). Модуль решает, КАК сеанс узнаёт о потере
// страницы и каким путём её провожает (`abandon` → `tab_gone` → закрытие; `tab_gone` и конец;
// сирота). Состоянием сеанса (фаза, сокет, порт, запись «сеанс выдан, страницы нет») по-прежнему
// владеет сам сеанс: сюда оно приходит только через узкий шов `PageLeavePorts`.

import type { ProtocolVersion, TabGoneReason } from "../../protocol/index.ts";
import { TOP_FRAME_ID } from "./handoff-session-basics.ts";
import type { HandoffSessionEnd } from "./session-contract.ts";
import type { SessionPageReturn } from "./page-return/session-page-return.ts";
import type { PagePresence } from "./session-page-presence.ts";
import type { SessionPhase } from "./session-phase.ts";
import type { ShadowSilenceWatch } from "./shadow-silence-watch.ts";
import type { VideoRelocation } from "./video-relocation.ts";

/** Что уходу страницы нужно от сеанса (его состояние остаётся у сеанса). */
export interface PageLeavePorts {
  readonly presence: PagePresence;
  readonly pageReturn: SessionPageReturn;
  readonly relocation: Pick<VideoRelocation, "nextVideoExpectedAfterLoss">;
  readonly silenceWatch: Pick<ShadowSilenceWatch, "stop">;
  phase(): SessionPhase;
  /** Фрейм живого порта (главный документ вкладки — `TOP_FRAME_ID`). */
  portFrameId(): number;
  /** Сокет к плееру уже открыт (`opening` без сокета ждёт выдачи сеанса). */
  socketOpened(): boolean;
  /** Плеер ответил `attached` на наш `attach`. */
  socketAttached(): boolean;
  agreedProtocol(): ProtocolVersion;
  /** `tab_gone{reason}` плееру и закрытие сокета (4002). */
  sendTabGoneAndClose(reason: TabGoneReason): void;
  /** Порт к странице уже мёртв: сеанс забывает его, не отсоединяя повторно. */
  forgetFramePort(): void;
  /** Сеанс выдан, а страницы для него нет: `attach` → `tab_gone` → закрытие (владеет сеанс). */
  abandonGrantedSession(reason: TabGoneReason, failure: string): void;
  /** Запись «сеанс выдан, страницы нет» ждёт `attached`; причину `tab_gone` в ней можно сменить. */
  isAbandonmentPending(): boolean;
  relabelAbandonmentAsClosed(): void;
  /** Страница убрала видео, а переезд открыт: ждать следующее. */
  waitForNextVideo(): void;
  finish(end: HandoffSessionEnd): void;
}

/** Входы ухода страницы; всё остальное — внутреннее дело модуля. */
export interface PageLeave {
  /** Порт к странице мёртв (его `onDisconnect` или команда в него не ушла). */
  endForLostFramePort(): void;
  /** Страница ушла по этой причине. */
  leavePage(reason: TabGoneReason): void;
  /** Браузер сообщил о закрытии вкладки (`tabs.onRemoved`). */
  notifyTabRemoved(): void;
}

export function createPageLeave(ports: PageLeavePorts): PageLeave {
  /**
   * Порт к странице мёртв: пришёл его `onDisconnect` или команда в него не ушла (Д-7 — тогда
   * `onDisconnect` может и не прийти, а плеер ждал бы подтверждения перемотки до 60 с).
   */
  function endForLostFramePort(): void {
    const phase = ports.phase();
    if (phase === "finished" || phase === "page_away") return;
    if (phase === "beginning") {
      ports.abandonGrantedSession(ports.presence.wasRemoved() ? "closed" : "navigated_away", "page_navigated_away");
      return;
    }
    if (ports.presence.wasRemoved()) {
      leavePage("closed");
      return;
    }
    if (framePortLossIsSurvivable()) {
      askWhetherPageStayed();
      return;
    }
    // Порт закрывается раньше, чем приходит tabs.onRemoved: живость вкладки спрашиваем у браузера.
    ports.presence.askWhyPageLeft(leavePage);
  }

  /**
   * Порт рекламного фрейма умер. Убрали ролик — ждём следующий. Человек ушёл на другой адрес — видно
   * по адресу вкладки: фрейм умер вместе со страницей, и новый сайт сеанс подхватывать не должен.
   */
  function askWhetherPageStayed(): void {
    ports.forgetFramePort();
    ports.silenceWatch.stop();
    ports.presence.askWhetherPageStayed((answer) => {
      const phase = ports.phase();
      if (phase !== "active" && phase !== "between_videos" && phase !== "moving") return;
      if (answer !== "same_page") leavePage(answer);
      else if (phase !== "between_videos") ports.waitForNextVideo();
    });
  }

  /**
   * Рекламный фрейм убрали вместе с роликом: это конец ролика, а не страницы. Главный документ вкладки
   * так не теряется: его порт умер, значит, страница ушла.
   */
  function framePortLossIsSurvivable(): boolean {
    if (ports.portFrameId() === TOP_FRAME_ID) return false;
    const phase = ports.phase();
    return phase === "between_videos" || phase === "moving" || (phase === "active" && ports.relocation.nextVideoExpectedAfterLoss());
  }

  /**
   * Страница ушла. Вернувшаяся вкладка ушла раньше тени — отказ возврата. Вернуть можно (11b2) — сирота
   * (`page_left` и фаза `page_away` через порт `sendPageLeft`); иначе прежний `tab_gone`.
   */
  function leavePage(reason: TabGoneReason): void {
    const phase = ports.phase();
    // В сироте страницы нет: запоздавший ответ браузера о старой вкладке сироту не кончает.
    if (phase === "finished" || phase === "page_away") return;
    if (ports.pageReturn.isReturning()) return ports.pageReturn.shadowDidNotStand();
    if (phase !== "ending" && ports.socketAttached() && ports.pageReturn.pageLeft(reason, ports.agreedProtocol()) === "orphaned") return;
    ports.sendTabGoneAndClose(reason);
    ports.finish({ kind: "page_gone", reason });
  }

  function notifyTabRemoved(): void {
    ports.presence.markRemoved();
    const phase = ports.phase();
    // В сироте старая вкладка уже не наша; при возврате закрытие новой — отказ «страница не загрузилась».
    if (phase === "page_away" || ports.pageReturn.isReturning()) return ports.pageReturn.tabRemoved(ports.presence.tabId());
    if (phase === "beginning" || (phase === "opening" && ports.socketOpened())) {
      ports.abandonGrantedSession("closed", "tab_closed");
    } else if (phase === "abandoning" && ports.isAbandonmentPending()) {
      ports.relabelAbandonmentAsClosed();
    } else if (phase === "active" || phase === "ending" || phase === "between_videos" || phase === "moving") {
      leavePage("closed");
    }
    // opening без сокета: openPlayerSession увидит флаг, когда плеер выдаст сеанс.
  }

  return { endForLostFramePort, leavePage, notifyTabRemoved };
}
