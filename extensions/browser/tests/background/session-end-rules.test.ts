// Чистые правила концов сеанса 11a (session-end-rules.ts): каждое — на всех своих ветках.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  closingAfterConfirmedEnd,
  endCommandAfterPlayerSocketEnd,
  stepAfterPageEndedShadow,
  tabGoneReasonForPagePlayerFailure,
} from "../../src/background/handoff/session-end-rules.ts";
import { END_WITHOUT_PLAYER, RETURN_PAUSED_AT_SHADOW } from "../../src/background/handoff/session-page-commands.ts";
import type { SocketEndCause } from "../../src/background/handoff/session-socket.ts";
import { closeReasonFromCode, PROTOCOL_VERSION_1_0, PROTOCOL_VERSION_1_2, PROTOCOL_VERSION_1_3, PROTOCOL_VERSION_1_8, PROTOCOL_VERSION_1_9, SESSION_CLOSE_CODES } from "../../src/protocol/index.ts";

const closedWith = (code: number): SocketEndCause => ({ kind: "player_closed", code, reason: closeReasonFromCode(code) });

describe("чем снять тень после конца сокета (решение 238)", () => {
  it("окно закрыто зрителем, заменено другой вкладкой или плеер выходит — пауза на позиции тени", () => {
    for (const code of [4000, 4001, 4006]) assert.deepEqual(endCommandAfterPlayerSocketEnd(closedWith(code)), RETURN_PAUSED_AT_SHADOW, String(code));
    assert.deepEqual(RETURN_PAUSED_AT_SHADOW.resume, { kind: "exactly", resume: "paused" });
    assert.deepEqual(RETURN_PAUSED_AT_SHADOW.position, { kind: "shadow_position" });
  });

  it("аварии и прочие коды — как прежде, «как тень»", () => {
    const others: SocketEndCause[] = [
      closedWith(1006),
      closedWith(1000),
      closedWith(4003),
      closedWith(4999),
      { kind: "attach_timeout" },
      { kind: "queue_overflow" },
      { kind: "player_protocol_violation", detail: "x" },
      { kind: "page_message_rejected", detail: "x" },
    ];
    for (const cause of others) assert.deepEqual(endCommandAfterPlayerSocketEnd(cause), END_WITHOUT_PLAYER, JSON.stringify(cause));
  });
});

describe("причина tab_gone для сломавшегося плеера страницы (решение 237)", () => {
  it("1.3 знает page_player_failed; 1.2 и 1.0 получают navigated_away", () => {
    assert.equal(tabGoneReasonForPagePlayerFailure(PROTOCOL_VERSION_1_3), "page_player_failed");
    assert.equal(tabGoneReasonForPagePlayerFailure({ major: 1, minor: 7 }), "page_player_failed", "новее 1.3 — тоже знает");
    assert.equal(tabGoneReasonForPagePlayerFailure(PROTOCOL_VERSION_1_2), "navigated_away");
    assert.equal(tabGoneReasonForPagePlayerFailure(PROTOCOL_VERSION_1_0), "navigated_away");
  });
});

describe("закрытие сокета после подтверждённого снятия тени", () => {
  it("«Return» — 4010 сразу; отказ поколения — 4003 после очереди; возврат плеера — 1000", () => {
    assert.deepEqual(closingAfterConfirmedEnd({ kind: "returned_by_viewer" }), { kind: "close_now", code: SESSION_CLOSE_CODES.viewer_returned_to_page });
    assert.equal(SESSION_CLOSE_CODES.viewer_returned_to_page, 4010);
    assert.deepEqual(closingAfterConfirmedEnd({ kind: "stream_rejected" }), { kind: "deliver_queue_then_close", reason: "protocol_violation" });
    assert.deepEqual(closingAfterConfirmedEnd({ kind: "returned_to_page" }), { kind: "close_now", code: 1000 });
  });
});

describe("страница сама сняла активную тень (вынесено из сеанса в 11b2)", () => {
  it("переезд открыт — ждать следующее видео, и при ошибке рекламы перед фильмом тоже", () => {
    for (const cause of ["element_released", "page_player_failed"] as const) {
      assert.deepEqual(stepAfterPageEndedShadow(cause, { nextVideoExpected: true, pageAddress: "unchanged", agreedProtocol: PROTOCOL_VERSION_1_3 }), { kind: "wait_for_next_video" }, cause);
    }
  });

  it("плеер сайта сломался — уход страницы с причиной по версии плеера (решение 237)", () => {
    assert.deepEqual(stepAfterPageEndedShadow("page_player_failed", { nextVideoExpected: false, pageAddress: "unchanged", agreedProtocol: PROTOCOL_VERSION_1_3 }), { kind: "leave_page", reason: "page_player_failed" });
    assert.deepEqual(stepAfterPageEndedShadow("page_player_failed", { nextVideoExpected: false, pageAddress: "unchanged", agreedProtocol: PROTOCOL_VERSION_1_2 }), { kind: "leave_page", reason: "navigated_away" });
  });

  it("страница забрала видео — 4009; флуд и сбой внутри страницы — 4003 с разной причиной", () => {
    assert.deepEqual(stepAfterPageEndedShadow("element_released", { nextVideoExpected: false, pageAddress: "unchanged", agreedProtocol: PROTOCOL_VERSION_1_3 }), {
      kind: "end_on_page_side",
      closeReason: "page_took_video_back",
      end: { kind: "page_took_video_back" },
    });
    assert.deepEqual(stepAfterPageEndedShadow("page_flood", { nextVideoExpected: true, pageAddress: "unchanged", agreedProtocol: PROTOCOL_VERSION_1_3 }), {
      kind: "end_on_page_side",
      closeReason: "protocol_violation",
      end: { kind: "page_flood", overBudget: "forwarded_bytes" },
    });
    assert.deepEqual(stepAfterPageEndedShadow("returned", { nextVideoExpected: true, pageAddress: "unchanged", agreedProtocol: PROTOCOL_VERSION_1_3 }), {
      kind: "end_on_page_side",
      closeReason: "protocol_violation",
      end: { kind: "page_side_failure" },
    });
  });
});

describe("потеря источника после конца серии и уход одностраничника (16a, решения 307, 42)", () => {
  const sameAddress = { nextVideoExpected: true, pageAddress: "unchanged", agreedProtocol: PROTOCOL_VERSION_1_9 } as const;

  it("тень не дождалась следующей серии: конец сразу (второго ожидания нет), 4011 плееру 1.9", () => {
    assert.deepEqual(stepAfterPageEndedShadow("next_episode_not_started", sameAddress), {
      kind: "end_on_page_side",
      closeReason: "next_episode_not_started",
      end: { kind: "next_episode_not_started" },
    });
    assert.equal(SESSION_CLOSE_CODES.next_episode_not_started, 4011);
  });

  it("старый плеер (1.8) того же конца не знает: прежний 4009, но конец тот же", () => {
    assert.deepEqual(stepAfterPageEndedShadow("next_episode_not_started", { ...sameAddress, agreedProtocol: PROTOCOL_VERSION_1_8 }), {
      kind: "end_on_page_side",
      closeReason: "page_took_video_back",
      end: { kind: "next_episode_not_started" },
    });
  });

  it("адрес вкладки сменился с начала видео — уход одностраничника, и после конца серии, и посреди", () => {
    const moved = { nextVideoExpected: false, pageAddress: "changed", agreedProtocol: PROTOCOL_VERSION_1_9 } as const;
    assert.deepEqual(stepAfterPageEndedShadow("next_episode_not_started", moved), { kind: "page_navigated_away" });
    assert.deepEqual(stepAfterPageEndedShadow("element_released", moved), { kind: "page_navigated_away" });
  });

  it("адрес сменился, но переезд открыт — сначала ждём следующее видео; сломанный плеер сайта — прежний уход", () => {
    const movedButExpected = { nextVideoExpected: true, pageAddress: "changed", agreedProtocol: PROTOCOL_VERSION_1_9 } as const;
    assert.deepEqual(stepAfterPageEndedShadow("element_released", movedButExpected), { kind: "wait_for_next_video" });
    assert.deepEqual(stepAfterPageEndedShadow("page_player_failed", { ...movedButExpected, nextVideoExpected: false }), { kind: "leave_page", reason: "page_player_failed" });
  });
});
