// Тень ждёт следующую серию на том же элементе (сессия 16a, решения 303, 306–308): источник снят ПОСЛЕ того,
// как часы плеера сказали `ended`, — срок `NEXT_EPISODE_WAIT_MS` и свой конец `next_episode_not_started`;
// посреди серии — прежние 5 с и `element_released`. Проверяется то, что видят фон (события наружу) и
// страница (тень на элементе или элемент снова настоящий).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { flushMicrotasks } from "./fake-media-dom.ts";
import { createShadowHarness, playingVideo, type ShadowHarness } from "./shadow-harness.ts";
import { EMPTY_SOURCE_GRACE_MS, EmptySourceWatch, emptySourceLimitMs } from "../../../src/page/shadow/empty-source-watch.ts";
import { NEXT_EPISODE_WAIT_MS } from "../../../src/shared/next-episode-wait.ts";
import { parsePageEvent } from "../../../src/shared/page-message-validation.ts";

const TICK_MS = 250;
const EPISODE_END_S = 30;
const NEXT_EPISODE_URL = "blob:https://site.example/episode-2";

type Session = ReturnType<ShadowHarness["start"]>;

function handoffEnds(harness: ShadowHarness) {
  return harness.emitted.filter((event) => event.kind === "handoff_ended");
}

function waitNotices(harness: ShadowHarness) {
  return harness.emitted.filter((event) => event.kind === "next_episode_wait_began");
}

/** Тикать часами плеера `durationMs` (раз в 250 мс) в состоянии `state`. */
function tickFor(harness: ShadowHarness, session: Session, durationMs: number, state: "playing" | "ended"): void {
  for (let elapsedMs = 0; elapsedMs < durationMs; elapsedMs += TICK_MS) {
    harness.tick(session, EPISODE_END_S, state);
    harness.clock.advance(TICK_MS);
  }
}

/** Тень всё ещё на элементе: странице отвечает она (модель часов), а не настоящий элемент. */
function assertStillShadowed(video: { real: { currentTime: number }; currentTime: number }): void {
  video.real.currentTime = 777;
  assert.notEqual(video.currentTime, 777, "позиция — от тени, а не от настоящего элемента");
}

/** Серия доиграла у плеера, страница сняла источник (порядок YouTube после `ended`). */
function episodeEndedThenSourceRemoved(harness: ShadowHarness) {
  const video = playingVideo(harness.dom, 10);
  const session = harness.start(video);
  harness.tick(session, EPISODE_END_S, "ended");
  assert.equal(video.ended, true, "страница увидела конец серии");
  video.removeAttribute("src");
  video.srcObject = null;
  return { video, session };
}

describe("тень после конца серии: ждём следующую 60 с, а не 5 с", () => {
  it("снятый после `ended` источник: 5 с давно прошли — тень на месте; ровно на сроке — next_episode_not_started", () => {
    const harness = createShadowHarness();
    const { video, session } = episodeEndedThenSourceRemoved(harness);
    tickFor(harness, session, 2 * EMPTY_SOURCE_GRACE_MS, "ended");
    assert.deepEqual(handoffEnds(harness), [], "посреди серии здесь уже был бы конец");
    assertStillShadowed(video);
    // Дотикать до мгновения на тик раньше срока: конца нет.
    tickFor(harness, session, NEXT_EPISODE_WAIT_MS - 2 * EMPTY_SOURCE_GRACE_MS - TICK_MS, "ended");
    harness.tick(session, EPISODE_END_S, "ended");
    assert.deepEqual(handoffEnds(harness), [], `на ${NEXT_EPISODE_WAIT_MS - TICK_MS} мс — ещё ждём`);
    harness.clock.advance(TICK_MS);
    harness.tick(session, EPISODE_END_S, "ended");
    assert.deepEqual(handoffEnds(harness), [{ kind: "handoff_ended", cause: "next_episode_not_started" }]);
    video.real.currentTime = 5;
    assert.equal(video.currentTime, 5, "элемент снова настоящий");
    assert.equal(video.real.muted, false, "muted страницы восстановлен");
  });

  it("о начале ожидания тень говорит фону один раз, и событие проходит строгую проверку фона", () => {
    const harness = createShadowHarness();
    const { video, session } = episodeEndedThenSourceRemoved(harness);
    // Страница снимает источник ещё раз (load()): ожидание то же, второго события нет.
    video.load();
    video.srcObject = null;
    tickFor(harness, session, 2 * EMPTY_SOURCE_GRACE_MS, "ended");
    assert.deepEqual(waitNotices(harness), [{ kind: "next_episode_wait_began" }]);
    assert.deepEqual(parsePageEvent({ kind: "next_episode_wait_began" }), { kind: "next_episode_wait_began" });
    assert.equal(parsePageEvent({ kind: "next_episode_wait_began", patience_ms: 1 }), undefined, "лишние поля — подделка");
    assert.deepEqual(parsePageEvent({ kind: "handoff_ended", cause: "next_episode_not_started" }), { kind: "handoff_ended", cause: "next_episode_not_started" });
  });

  it("повторное снятие источника срок не продлевает: конец через 60 с от ПЕРВОГО снятия", () => {
    const harness = createShadowHarness();
    const { video, session } = episodeEndedThenSourceRemoved(harness);
    tickFor(harness, session, NEXT_EPISODE_WAIT_MS / 2, "ended");
    video.srcObject = null;
    tickFor(harness, session, NEXT_EPISODE_WAIT_MS / 2, "ended");
    harness.tick(session, EPISODE_END_S, "ended");
    assert.deepEqual(handoffEnds(harness), [{ kind: "handoff_ended", cause: "next_episode_not_started" }]);
  });

  it("часы плеера после снятия говорят `playing` (старое поколение) — срок всё равно 60 с: момент запомнен", () => {
    const harness = createShadowHarness();
    const { session } = episodeEndedThenSourceRemoved(harness);
    tickFor(harness, session, 2 * EMPTY_SOURCE_GRACE_MS, "playing");
    assert.deepEqual(handoffEnds(harness), []);
  });

  it("серия 2 новым MediaSource через 30 с: новое поколение, тень на месте и после срока, конца нет", async () => {
    const harness = createShadowHarness();
    const { video, session } = episodeEndedThenSourceRemoved(harness);
    tickFor(harness, session, NEXT_EPISODE_WAIT_MS / 2, "ended");
    harness.mediaSourceUrls.add(NEXT_EPISODE_URL);
    video.src = NEXT_EPISODE_URL;
    await flushMicrotasks();
    tickFor(harness, session, 2 * NEXT_EPISODE_WAIT_MS, "playing");
    assert.deepEqual(handoffEnds(harness), []);
    assertStillShadowed(video);
  });

  it("после `ended` страница поставила обычный файл — конец сразу element_released (ожидание дальше — у фона)", () => {
    const harness = createShadowHarness();
    const { video } = episodeEndedThenSourceRemoved(harness);
    video.src = "https://cdn.example/episode-2.mp4";
    assert.deepEqual(handoffEnds(harness), [{ kind: "handoff_ended", cause: "element_released" }]);
  });

  it("посреди серии — прежние 5 с и element_released, о следующей серии ни слова", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    harness.tick(session, EPISODE_END_S, "playing");
    video.removeAttribute("src");
    video.srcObject = null;
    tickFor(harness, session, EMPTY_SOURCE_GRACE_MS, "playing");
    harness.tick(session, EPISODE_END_S, "playing");
    assert.deepEqual(handoffEnds(harness), [{ kind: "handoff_ended", cause: "element_released" }]);
    assert.deepEqual(waitNotices(harness), []);
  });
});

describe("EmptySourceWatch: срок по моменту снятия, границы ровно на сроке", () => {
  const empty = () => "no_source" as const;

  for (const [moment, limitMs, cause] of [
    ["mid_episode", EMPTY_SOURCE_GRACE_MS, "release_to_page"],
    ["after_episode_end", NEXT_EPISODE_WAIT_MS, "next_episode_not_started"],
  ] as const) {
    it(`${moment}: за 1 мс до ${limitMs} мс — держать, ровно на сроке — ${cause}`, () => {
      const watch = new EmptySourceWatch();
      assert.equal(emptySourceLimitMs(moment), limitMs);
      watch.sourceRemoved(1_000, moment);
      assert.equal(watch.onClockTick(empty, 1_000 + limitMs - 1), "keep_shadow");
      assert.equal(watch.onClockTick(empty, 1_000 + limitMs), cause);
    });
  }

  it("первое снятие решает и срок, и момент: второе (другой момент) не меняет ничего", () => {
    const watch = new EmptySourceWatch();
    assert.equal(watch.sourceRemoved(0, "mid_episode"), "mid_episode_wait_began");
    assert.equal(watch.sourceRemoved(4_000, "after_episode_end"), "wait_continues");
    assert.equal(watch.onClockTick(empty, EMPTY_SOURCE_GRACE_MS), "release_to_page", "срок и причина — от первого снятия");
  });

  it("новый MediaSource снимает ожидание; следующее снятие — новое ожидание со своим моментом", () => {
    const watch = new EmptySourceWatch();
    assert.equal(watch.sourceRemoved(0, "after_episode_end"), "next_episode_wait_began");
    assert.equal(watch.onClockTick(() => "media_source", 10_000), "keep_shadow");
    assert.equal(watch.isWaiting, false);
    assert.equal(watch.sourceRemoved(20_000, "after_episode_end"), "next_episode_wait_began", "конец серии 2 — снова «начали ждать»");
    assert.equal(watch.onClockTick(() => "not_media_source", 20_001), "release_to_page", "чужой источник — конец сразу и после конца серии");
  });
});
