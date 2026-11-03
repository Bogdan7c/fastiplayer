// Переходы источника под тенью (Д-1, Д-8, Д-10): снятый источник — пауза решения страницы,
// новый MediaSource — новое поколение, чужой источник или пустота дольше допуска — честный конец.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { flushMicrotasks, recordEvents } from "./fake-media-dom.ts";
import { createShadowHarness, playingVideo, type ShadowHarness } from "./shadow-harness.ts";
import { EMPTY_SOURCE_GRACE_MS } from "../../../src/page/shadow/empty-source-watch.ts";

const MS2_URL = "blob:https://site.example/ms-2";

/** Страница создала адрес для нового MediaSource (реестр MAIN-мира знает его). */
function pageCreatedMediaSourceUrl(harness: ShadowHarness, url = MS2_URL): string {
  harness.mediaSourceUrls.add(url);
  return url;
}

function handoffEnds(harness: ShadowHarness) {
  return harness.emitted.filter((event) => event.kind === "handoff_ended");
}

/** Тень всё ещё на элементе: странице отвечает она, а не настоящий элемент. */
function assertStillShadowed(video: { real: { currentTime: number }; currentTime: number }, expectedShadowSeconds: number): void {
  video.real.currentTime = 777;
  assert.equal(video.currentTime, expectedShadowSeconds);
}

/** Тикать часами плеера `durationMs` раз в 250 мс со старой позицией. */
function tickFor(harness: ShadowHarness, session: ReturnType<ShadowHarness["start"]>, durationMs: number, positionSeconds = 30): void {
  for (let elapsedMs = 0; elapsedMs < durationMs; elapsedMs += 250) {
    harness.tick(session, positionSeconds, "playing");
    harness.clock.advance(250);
  }
}

describe("тень: источник снят на мгновение — пауза решения, а не конец (Д-1, Д-10)", () => {
  it("YouTube: removeAttribute(src), srcObject = null, затем src = новый MediaSource — тень на месте, 0 и пауза, конца нет", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    harness.tick(session, 30, "playing");
    video.removeAttribute("src");
    video.srcObject = null;
    assert.deepEqual(handoffEnds(harness), [], "снятый источник ещё не конец");
    assert.equal(video.paused, true);
    assert.equal(video.currentTime, 0);
    video.src = pageCreatedMediaSourceUrl(harness);
    await flushMicrotasks();
    tickFor(harness, session, 2 * EMPTY_SOURCE_GRACE_MS);
    assert.deepEqual(handoffEnds(harness), []);
    assert.equal(video.paused, true);
    assert.equal(video.currentTime, 0, "часы старого поколения не сдвигают тень");
    assertStillShadowed(video, 0);
  });

  it("removeAttribute(src); load() → src = blob MediaSource — тень на месте", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    harness.tick(session, 30, "playing");
    video.removeAttribute("src");
    video.load();
    assert.deepEqual(handoffEnds(harness), []);
    video.src = pageCreatedMediaSourceUrl(harness);
    await flushMicrotasks();
    tickFor(harness, session, 2 * EMPTY_SOURCE_GRACE_MS);
    assert.deepEqual(handoffEnds(harness), []);
    assert.equal(video.paused, true);
    assertStillShadowed(video, 0);
  });

  it("src = \"\" (геттер отдаёт адрес документа) → src = blob MediaSource — тень на месте", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    video.src = "";
    assert.deepEqual(handoffEnds(harness), [], "пустой атрибут — это «источника нет», а не обычный файл");
    video.src = pageCreatedMediaSourceUrl(harness);
    await flushMicrotasks();
    tickFor(harness, session, 2 * EMPTY_SOURCE_GRACE_MS);
    assert.deepEqual(handoffEnds(harness), []);
    assertStillShadowed(video, 0);
  });

  it("play() страницы, пока источника нет, переживает новый src: браузер не сбрасывает пустой элемент", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    harness.start(video);
    video.removeAttribute("src");
    video.load();
    await flushMicrotasks();
    const played = video.play();
    let rejected = false;
    played.catch(() => {
      rejected = true;
    });
    video.src = pageCreatedMediaSourceUrl(harness);
    await flushMicrotasks();
    assert.equal(video.paused, false, "элемент в NETWORK_EMPTY: алгоритм загрузки не ставит паузу");
    assert.equal(rejected, false, "и не отвергает ожидающий play()");
  });
});

describe("тень: источник убран насовсем — честный конец по допуску (Д-8)", () => {
  it("srcObject = null и тишина: до допуска конца нет, после — element_released", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    video.removeAttribute("src");
    video.srcObject = null;
    tickFor(harness, session, EMPTY_SOURCE_GRACE_MS - 250);
    harness.tick(session, 30, "playing");
    assert.deepEqual(handoffEnds(harness), [], `до допуска (${EMPTY_SOURCE_GRACE_MS} мс) — нет`);
    harness.clock.advance(250);
    harness.tick(session, 30, "playing");
    assert.deepEqual(handoffEnds(harness), [{ kind: "handoff_ended", cause: "element_released" }]);
    assert.equal(video.real.muted, false, "muted страницы восстановлен");
    assert.deepEqual(harness.guardCalls, ["suspend", "restore"]);
    video.real.currentTime = 5;
    assert.equal(video.currentTime, 5, "элемент снова настоящий");
  });

  it("пусто → setAttribute(src, blob MediaSource) без emptied: тень замечает новый источник на тике, конца нет", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    video.removeAttribute("src");
    video.load();
    await flushMicrotasks();
    const page = recordEvents(video, ["emptied"]);
    // Элемент в NETWORK_EMPTY: алгоритм загрузки не шлёт emptied, переопределения не видят атрибут.
    video.setAttribute("src", pageCreatedMediaSourceUrl(harness));
    await flushMicrotasks();
    assert.deepEqual(page.seen, [], "браузер молчит");
    tickFor(harness, session, 2 * EMPTY_SOURCE_GRACE_MS);
    assert.deepEqual(handoffEnds(harness), []);
    assertStillShadowed(video, 0);
  });

  it("пусто → обычный файл сеттером: element_released сразу", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    harness.start(video);
    video.removeAttribute("src");
    video.load();
    video.src = "https://cdn.example/movie.mp4";
    assert.deepEqual(handoffEnds(harness), [{ kind: "handoff_ended", cause: "element_released" }]);
  });

  it("пусто → обычный файл через setAttribute (без emptied): element_released на ближайшем тике", async () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    video.removeAttribute("src");
    video.load();
    await flushMicrotasks();
    video.setAttribute("src", "https://cdn.example/movie.mp4");
    await flushMicrotasks();
    assert.deepEqual(handoffEnds(harness), []);
    harness.tick(session, 30, "playing");
    assert.deepEqual(handoffEnds(harness), [{ kind: "handoff_ended", cause: "element_released" }]);
  });

  it("допуск не продлевается повторным снятием источника: страница не держит тень вечно", () => {
    const harness = createShadowHarness();
    const video = playingVideo(harness.dom, 10);
    const session = harness.start(video);
    video.removeAttribute("src");
    video.srcObject = null;
    tickFor(harness, session, EMPTY_SOURCE_GRACE_MS / 2);
    video.srcObject = null;
    tickFor(harness, session, EMPTY_SOURCE_GRACE_MS / 2);
    harness.tick(session, 30, "playing");
    assert.deepEqual(handoffEnds(harness), [{ kind: "handoff_ended", cause: "element_released" }]);
  });
});
