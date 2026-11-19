// Реакции сеанса передачи на плеер: сообщения плеера (команды тени, возврат страницы, адаптеры сайта, байты
// источников) и конец сокета плеера.
//
// Вынесено из `handoff-session.ts` без изменения поведения (сессия 16b3: файл упёрся в предел 800 строк).
// Модуль решает, ЧТО сеанс делает на каждое сообщение плеера в каждой фазе и чем кончается обрыв сокета.
// Состоянием сеанса (фаза, сокет, порт, `pendingEnd`, `abandoned`) по-прежнему владеет сам сеанс: сюда оно
// приходит только через узкий шов `PlayerReactionPorts` — вопросы и намерения, а не поля.

import type { HandOffTabOutcome, PageCommand } from "../../shared/handoff-contract.ts";
import type { PlayerSessionMessage } from "../../protocol/index.ts";
import type { CurrentVideoGeneration } from "./current-video-generation.ts";
import type { PageDelivery } from "./handoff-session-basics.ts";
import { translatePlayerMessage } from "./player-message-translator.ts";
import type { SessionPageReveal } from "./page-tab-reveal.ts";
import type { SessionPageReturn } from "./page-return/session-page-return.ts";
import type { HandoffSessionEnd } from "./session-contract.ts";
import { describeSocketEnd } from "./session-outcome-words.ts";
import { END_WITHOUT_PLAYER } from "./session-page-commands.ts";
import { endCommandAfterPlayerSocketEnd, stepAfterPlayerSocketEnded } from "./session-end-rules.ts";
import type { SessionPhase } from "./session-phase.ts";
import type { SessionSiteAdapters } from "./session-site-adapters.ts";
import type { SessionSourceFeeds } from "./session-source-feeds.ts";
import type { SessionSocket, SocketEndCause } from "./session-socket.ts";
import type { VideoRelocation } from "./video-relocation.ts";

/** Что реакциям на плеер нужно от сеанса (его состояние остаётся у сеанса). */
export interface PlayerReactionPorts {
  readonly pageReturn: Pick<SessionPageReturn, "notePlayerSecond" | "reopenRequested" | "isReturning">;
  readonly videoGeneration: CurrentVideoGeneration;
  readonly siteAdapters: Pick<SessionSiteAdapters, "playerClock" | "command">;
  readonly relocation: Pick<VideoRelocation, "noteClockState">;
  readonly pageReveal: SessionPageReveal;
  readonly feeds: Pick<SessionSourceFeeds, "requestResource" | "requestRange">;
  /** Сокет к плееру (`undefined` — ещё не открыт). */
  socket(): SessionSocket | undefined;
  phase(): SessionPhase;
  /** Журнал разработчика. */
  note(message: string): void;
  /** Команда, потеря которой рвёт согласие с тенью (Д-7). */
  commandPage(command: PageCommand): void;
  /** Отправка странице без последствий. */
  tryPostToPage(command: PageCommand): PageDelivery;
  /** Снять тень и ждать подтверждения страницы (только в фазе `active`). */
  endHandoff(command: Extract<PageCommand, { kind: "end_handoff" }>, end: HandoffSessionEnd): void;
  /** Тени нет, снимать нечего: запомнить конец и закрыть сокет сразу. */
  endWithoutShadow(end: HandoffSessionEnd): void;
  /** Старт не удался: ответ на `hand_off_tab` и закрытие. */
  failStart(outcome: HandOffTabOutcome): void;
  /** Ответ «сеанс выдан, страницы нет», если такая запись есть. */
  abandonedOutcome(): HandOffTabOutcome | undefined;
  finish(end: HandoffSessionEnd): void;
}

/** Входы от плеера; всё остальное — внутреннее дело модуля. */
export interface PlayerReactions {
  /** Сообщение плеера по сокету. */
  handlePlayerMessage(message: PlayerSessionMessage): void;
  /** Сокет к плееру кончился (закрытие нами отфильтровывается внутри). */
  handleSocketEnded(cause: SocketEndCause): void;
}

export function createPlayerReactions(ports: PlayerReactionPorts): PlayerReactions {
  function handlePlayerMessage(message: PlayerSessionMessage): void {
    // Секунда плеера нужна возврату страницы во всех фазах (11b2): в сироте часы — единственная новость. Часы прошлого
    // видео (16a2) тоже: пока вернувшаяся страница встаёт, плеер играет прежнее поколение — тот самый фильм.
    if (message.type === "clock") ports.pageReturn.notePlayerSecond(message.position_us);
    // Память выбора решает по часам, у начала ли серия (14b2); в сироте её слушатели и так молчат. Часы прошлого
    // видео (16a2) — секунда чужого ролика: конец рекламы на 12 с не значит «серия уже на 12-й секунде».
    if (message.type === "clock" && ports.videoGeneration.clockRelationOf(message.generation) !== "previous_video") ports.siteAdapters.playerClock(message.position_us, message.state);
    if (ports.phase() === "page_away") {
      if (message.type === "reopen_page") ports.pageReturn.reopenRequested();
      if (message.type === "show_page") ports.note("show_page ignored: the page is away");
      return;
    }
    // До старта тени и после её снятия командовать некем; в `page_reloading` (16c) — часы, конец и байты.
    if (ports.phase() !== "active" && ports.phase() !== "between_videos" && ports.phase() !== "moving" && ports.phase() !== "page_reloading") return;
    const action = translatePlayerMessage(message, ports.videoGeneration);
    switch (action.kind) {
      case "shadow":
        // Пока тень встаёт в вернувшейся вкладке, часы старого видео переезду не указ (он остановлен).
        if (action.command.kind === "clock" && !ports.pageReturn.isReturning()) ports.relocation.noteClockState(action.command);
        // Между роликами часы некому показывать: тени нет.
        if (ports.phase() === "active") ports.commandPage({ kind: "shadow", command: action.command });
        return;
      case "end_handoff": {
        const end: HandoffSessionEnd = action.cause === "returned_to_page" ? { kind: "returned_to_page" } : { kind: "stream_rejected" };
        if (ports.phase() === "active") {
          ports.endHandoff({ kind: "end_handoff", position: action.position, resume: action.resume }, end);
        } else {
          // Тени нет, снимать нечего и ждать подтверждения не от кого: зритель закрыл окно между роликами.
          ports.endWithoutShadow(end);
        }
        return;
      }
      case "site_adapter": return ports.phase() === "page_reloading" ? ports.note("site adapter command ignored: the page is reloading") : void ports.siteAdapters.command(action.command);
      case "show_page": return void ports.pageReveal.show(); // сбои разобраны внутри, наружу не бросает
      case "fetch_resource": {
        // Ресурсы HLS просят только в поколении HLS; в другом поколении фид сам ответит `refused`.
        const socket = ports.socket();
        if (socket !== undefined) ports.feeds.requestResource(action.request, socket);
        return;
      }
      case "fetch_range":
        // Байты файла просят только в поколении-файле. Без файла запрос не наш (плеер ошибся
        // сам): его чтение честно истечёт у плеера, сеанс страницы это не ломает.
        ports.feeds.requestRange(action.request);
        return;
      case "attached":
      case "buffer_intake":
      case "ignore":
        return;
    }
  }

  function handleSocketEnded(cause: SocketEndCause): void {
    if (cause.kind === "closed_by_extension") return; // мы сами закрыли — решение уже принято
    const playerEnded: HandoffSessionEnd = { kind: "player_ended", cause };
    switch (stepAfterPlayerSocketEnded(ports.phase())) { // таблица по фазам — session-end-rules.ts
      case "fail_start":
        return ports.failStart({ kind: "player_unavailable", reason: `session_socket:${describeSocketEnd(cause)}` });
      case "finish_abandoned":
        return ports.finish({ kind: "start_failed", outcome: ports.abandonedOutcome() ?? { kind: "failed", reason: "abandoned" } });
      case "end_shadow":
        return ports.endHandoff(endCommandAfterPlayerSocketEnd(cause), playerEnded);
      case "finish_without_shadow":
        // В сироте и в ожидании перезагрузки порта нет (команда никуда не уйдёт); открытие страницы отменит `finish`.
        ports.tryPostToPage(END_WITHOUT_PLAYER);
        return ports.finish(playerEnded);
      case "nothing":
        return;
    }
  }

  return { handlePlayerMessage, handleSocketEnded };
}
