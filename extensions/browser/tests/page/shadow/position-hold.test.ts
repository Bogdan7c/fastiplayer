// Правила удержания позиции тени (сессия 16a2): какой тик подтверждает прыжок в зависимости от того, к какому
// видео он относится. Чистые числа, без модели тени.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  MAX_UNCONFIRMED_TICKS,
  PAGE_SEEK_CONFIRM_TOLERANCE_US,
  SOURCE_RESET_GIVE_UP_MS,
  holdConfirmedBy,
  newPositionHold,
  noteUnconfirmedTick,
  type HoldTick,
  type PositionHold,
} from "../../../src/page/shadow/position-hold.ts";

const TARGET_US = 11_100_000;
const FAR_US = TARGET_US + 5 * PAGE_SEEK_CONFIRM_TOLERANCE_US;

const sourceReset = (): PositionHold => newPositionHold({ kind: "source_reset", targetUs: TARGET_US, landsAtEnd: false }, 0);
const tick = (positionUs: number, state: HoldTick["state"], relation: HoldTick["relation"]): HoldTick => ({ positionUs, state, relation });

describe("удержание source_reset: пометка тика решает (16a2)", () => {
  it("previous_video не подтверждает никогда: ни у цели, ни после признания, ни по страховке времени", () => {
    const acknowledged = { ...sourceReset(), playerAcknowledgedNewSource: true };
    for (const state of ["playing", "paused", "waiting", "ended"] as const) {
      assert.equal(holdConfirmedBy(sourceReset(), tick(TARGET_US, state, "previous_video"), 0), false, `у цели, ${state}`);
      assert.equal(holdConfirmedBy(acknowledged, tick(FAR_US, state, "previous_video"), 0), false, `после признания, ${state}`);
      assert.equal(holdConfirmedBy(sourceReset(), tick(FAR_US, state, "previous_video"), SOURCE_RESET_GIVE_UP_MS * 10), false, `страховка, ${state}`);
    }
  });

  it("this_video — прежние правила: у цели (даже ended), после признания, по страховке — но не ended вдали", () => {
    assert.equal(holdConfirmedBy(sourceReset(), tick(TARGET_US + PAGE_SEEK_CONFIRM_TOLERANCE_US, "playing", "this_video"), 0), true);
    assert.equal(holdConfirmedBy(sourceReset(), tick(TARGET_US, "ended", "this_video"), 0), true, "настоящий конец у цели");
    assert.equal(holdConfirmedBy(sourceReset(), tick(FAR_US, "playing", "this_video"), 0), false);
    assert.equal(holdConfirmedBy({ ...sourceReset(), playerAcknowledgedNewSource: true }, tick(FAR_US, "playing", "this_video"), 0), true);
    assert.equal(holdConfirmedBy(sourceReset(), tick(FAR_US, "playing", "this_video"), SOURCE_RESET_GIVE_UP_MS), true);
    assert.equal(holdConfirmedBy(sourceReset(), tick(FAR_US, "ended", "this_video"), SOURCE_RESET_GIVE_UP_MS), false);
  });

  it("unknown (сторона 1.9) — прежние правила плюс запасное: ended не подтверждает даже у цели и после признания", () => {
    assert.equal(holdConfirmedBy(sourceReset(), tick(TARGET_US, "ended", "unknown"), 0), false, "конец баннера на 12,0 с у старта фильма с 11,1 с");
    assert.equal(holdConfirmedBy({ ...sourceReset(), playerAcknowledgedNewSource: true }, tick(FAR_US, "ended", "unknown"), 0), false);
    assert.equal(holdConfirmedBy(sourceReset(), tick(FAR_US, "ended", "unknown"), SOURCE_RESET_GIVE_UP_MS), false);
    assert.equal(holdConfirmedBy(sourceReset(), tick(TARGET_US, "playing", "unknown"), 0), true);
    assert.equal(holdConfirmedBy({ ...sourceReset(), playerAcknowledgedNewSource: true }, tick(FAR_US, "playing", "unknown"), 0), true);
    assert.equal(holdConfirmedBy(sourceReset(), tick(FAR_US, "playing", "unknown"), SOURCE_RESET_GIVE_UP_MS - 1), false);
    assert.equal(holdConfirmedBy(sourceReset(), tick(FAR_US, "playing", "unknown"), SOURCE_RESET_GIVE_UP_MS), true);
  });

  it("проверка подтверждения ничего в удержании не меняет", () => {
    const hold = sourceReset();
    const before = { ...hold };
    holdConfirmedBy(hold, tick(FAR_US, "waiting", "this_video"), SOURCE_RESET_GIVE_UP_MS * 2);
    assert.deepEqual(hold, before);
  });
});

describe("учёт неподтвердившего тика (16a2)", () => {
  it("waiting признаёт новый источник у this_video и unknown, но не у previous_video", () => {
    for (const relation of ["this_video", "unknown"] as const) {
      const hold = sourceReset();
      noteUnconfirmedTick(hold, { state: "waiting", relation });
      assert.equal(hold.playerAcknowledgedNewSource, true, relation);
    }
    const hold = sourceReset();
    noteUnconfirmedTick(hold, { state: "waiting", relation: "previous_video" });
    assert.equal(hold.playerAcknowledgedNewSource, false, "«жду» прошлого видео — про чужой ролик");
    assert.equal(hold.unconfirmedTicksLeft, MAX_UNCONFIRMED_TICKS - 1, "счётчик терпения идёт у любого тика");
  });

  it("тик игры признания не ставит", () => {
    const hold = sourceReset();
    noteUnconfirmedTick(hold, { state: "playing", relation: "this_video" });
    assert.equal(hold.playerAcknowledgedNewSource, false);
  });
});

describe("перемотки: пометка не меняет прежних правил", () => {
  it("page_seek — у цели или после MAX_UNCONFIRMED_TICKS тиков, у любой пометки", () => {
    const pageSeek = newPositionHold({ kind: "page_seek", targetUs: TARGET_US, landsAtEnd: false }, 0);
    for (const relation of ["this_video", "previous_video", "unknown"] as const) {
      assert.equal(holdConfirmedBy(pageSeek, tick(TARGET_US, "playing", relation), 0), true, relation);
      assert.equal(holdConfirmedBy(pageSeek, tick(FAR_US, "playing", relation), 0), false, relation);
      assert.equal(holdConfirmedBy({ ...pageSeek, unconfirmedTicksLeft: 0 }, tick(FAR_US, "playing", relation), 0), true, relation);
    }
  });

  it("player_seek — первый же тик после команды", () => {
    const playerSeek = newPositionHold({ kind: "player_seek", targetUs: TARGET_US, landsAtEnd: false }, 0);
    assert.equal(holdConfirmedBy(playerSeek, tick(FAR_US, "playing", "previous_video"), 0), true);
  });
});
