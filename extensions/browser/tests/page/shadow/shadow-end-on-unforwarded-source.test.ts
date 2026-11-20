// Снятие тени, когда на том же элементе уже стоит чужой, не переданный плееру источник (16b3c, решение 381).
//
// Shorts ушёл на главную, и страница ставит превью главной новым MediaSource на ТОТ ЖЕ элемент. Фон это поколение
// плееру не пересылает и кончает сеанс (`END_WITHOUT_PLAYER` = позиция тени, игра как у тени). Плеер всё ещё шлёт
// часы ПРЕЖНЕГО ролика (например 12 с), и для фона они — «этого видео»: о новом поколении он плееру не говорил.
// Страница должна получить элемент так, как поставила превью сама: с её позиции (обычно 0) и в её состоянии
// игры, а не на секунде прежнего ролика Shorts.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { flushMicrotasks } from "./fake-media-dom.ts";
import { createShadowHarness, playingVideo, type ShadowHarness } from "./shadow-harness.ts";
import type { FakeVideo } from "./fake-media-dom.ts";
import type { ShadowSession } from "../../../src/page/shadow/index.ts";

/** Секунда прежнего ролика Shorts в окне, когда страница ушла на главную. */
const SHORTS_REEL_SECOND = 12;
/** Адрес нового MediaSource превью главной. */
const PREVIEW_MEDIA_SOURCE_URL = "blob:https://www.youtube.com/preview-ms";

/** Тень стоит на ролике Shorts, который играет в окне на `SHORTS_REEL_SECOND`. */
function shadowOnShortsReel(): { harness: ShadowHarness; video: FakeVideo; session: ShadowSession } {
  const harness = createShadowHarness();
  const video = playingVideo(harness.dom, SHORTS_REEL_SECOND);
  const session = harness.start(video);
  harness.tick(session, SHORTS_REEL_SECOND, "playing", 1000, "this_video");
  return { harness, video, session };
}

/** Страница поставила превью главной новым MediaSource на тот же элемент. */
function pageAttachesPreview(harness: ShadowHarness, video: FakeVideo): void {
  harness.mediaSourceUrls.add(PREVIEW_MEDIA_SOURCE_URL);
  video.src = PREVIEW_MEDIA_SOURCE_URL;
}

/** Плеер ещё шлёт часы прежнего ролика: для фона это «это видео» — о превью он плееру не говорил. */
function playerStillTicksTheShortsReel(harness: ShadowHarness, session: ShadowSession, state: "playing" | "paused"): void {
  harness.clock.advance(250);
  harness.tick(session, SHORTS_REEL_SECOND + 0.25, state, 1000, "this_video");
}

/** Записи настоящему элементу `currentTime` после мига `fromCall` (номер вызова в журнале фейка). */
function realPositionWritesAfter(video: FakeVideo, fromCall: number): number {
  return video.realCalls.slice(fromCall).filter((call) => call.kind === "set" && call.property === "currentTime").length;
}

describe("снятие тени на чужом, не переданном источнике того же элемента", () => {
  it("превью с 0 и с play() страницы: элемент отдан на 0 и играет — секунда ролика Shorts его не трогает", async () => {
    const { harness, video, session } = shadowOnShortsReel();
    pageAttachesPreview(harness, video);
    const callsAfterAttach = video.realCalls.length;
    let playOutcome = "pending";
    video.play().then(
      () => (playOutcome = "resolved"),
      (error: Error) => (playOutcome = `rejected: ${error.name}`),
    );
    playerStillTicksTheShortsReel(harness, session, "playing");
    playerStillTicksTheShortsReel(harness, session, "paused"); // сеанс уже переслал плееру паузу ухода (370)
    session.end({ kind: "shadow_position" }, { kind: "as_shadow" });
    assert.equal(video.real.currentTime, 0, "превью начинается так, как его поставила страница");
    assert.equal(realPositionWritesAfter(video, callsAfterAttach), 0, "позицию превью тень не переставляла");
    assert.equal(video.real.paused, false, "страница звала play() превью — оно играет");
    assert.deepEqual(harness.emitted.at(-1), { kind: "handoff_ended", cause: "returned" });
    await flushMicrotasks();
    assert.equal(playOutcome.startsWith("rejected"), false, `обещание play() страницы не отвергнуто: ${playOutcome}`);
  });

  it("страница поставила превью на свою секунду (3 с) — элемент отдан на 3 с, а не на секунде Shorts", () => {
    const { harness, video, session } = shadowOnShortsReel();
    pageAttachesPreview(harness, video);
    video.currentTime = 3;
    playerStillTicksTheShortsReel(harness, session, "paused");
    session.end({ kind: "shadow_position" }, { kind: "as_shadow" });
    assert.equal(video.real.currentTime, 3);
  });

  it("страница превью не запускала — элемент отдан на паузе (решение страницы), на 0", () => {
    const { harness, video, session } = shadowOnShortsReel();
    pageAttachesPreview(harness, video);
    playerStillTicksTheShortsReel(harness, session, "playing");
    session.end({ kind: "shadow_position" }, { kind: "as_shadow" });
    assert.equal(video.real.paused, true);
    assert.equal(video.real.currentTime, 0);
  });
});
