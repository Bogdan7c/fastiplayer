// Вид ожидания переезда после конца ролика (16a): серия ждёт долго, реклама и ролик «может быть рекламой» —
// коротко. Находка полного прогона стенда 16a: страница `ad-no-film` (короткая неузнанная реклама без фильма)
// ждала 60 с вместо прежних 15 с и окно не закрывалось в срок.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createVideoRelocation, NEXT_VIDEO_WAIT_MS, type NextVideoWait, type RelocationVideo } from "../../src/background/handoff/video-relocation.ts";
import { NEXT_EPISODE_WAIT_MS } from "../../src/shared/next-episode-wait.ts";
import { createFakeClock } from "./support.ts";

/** Ролик доиграл у плеера и страница его убрала: какое ожидание началось и когда оно истекло. */
function waitAfterEnded(video: RelocationVideo): { readonly started: NextVideoWait; readonly expiredAfterMs: number | undefined; readonly episodeEnded: boolean } {
  const clock = createFakeClock();
  let expiredAtMs: number | undefined;
  let elapsedMs = 0;
  const relocation = createVideoRelocation({
    scheduler: clock.scheduler,
    armFrames: () => undefined,
    onWaitExpired: () => {
      expiredAtMs = elapsedMs;
    },
  });
  relocation.videoStarted(video);
  relocation.noteClockState({ state: "ended", relation: "this_video" });
  const episodeEnded = relocation.episodeEnded();
  const started = relocation.videoLeft();
  // Шаг в секунду до чуть дальше долгого срока: видно, на какой секунде истекло.
  while (expiredAtMs === undefined && elapsedMs <= NEXT_EPISODE_WAIT_MS + 1_000) {
    // Сначала сдвигаем отметку: срабатывание случается внутри `advance` и читает уже новую секунду.
    elapsedMs += 1_000;
    clock.advance(1_000);
  }
  relocation.stop();
  return { started, expiredAfterMs: expiredAtMs, episodeEnded };
}

describe("вид ожидания после конца ролика (16a)", () => {
  it("доиграла серия (фильм, не «может быть рекламой») — долгое ожидание 60 с", () => {
    const outcome = waitAfterEnded({ role: "film", mayBeAd: false });
    assert.equal(outcome.started, "after_episode_end");
    assert.equal(outcome.episodeEnded, true);
    assert.equal(outcome.expiredAfterMs, NEXT_EPISODE_WAIT_MS);
  });

  it("доиграл короткий ролик «может быть рекламой» — прежние 15 с, это не серия", () => {
    const outcome = waitAfterEnded({ role: "film", mayBeAd: true });
    assert.equal(outcome.started, "next_video");
    assert.equal(outcome.episodeEnded, false);
    assert.equal(outcome.expiredAfterMs, NEXT_VIDEO_WAIT_MS);
  });

  it("доиграла узнанная реклама — прежние 15 с", () => {
    const outcome = waitAfterEnded({ role: "ad", mayBeAd: false });
    assert.equal(outcome.started, "next_video");
    assert.equal(outcome.episodeEnded, false);
    assert.equal(outcome.expiredAfterMs, NEXT_VIDEO_WAIT_MS);
  });
});

describe("сеанс-лента потерял ролик без ухода прокруткой (16b3, F2)", () => {
  function relocationWithExpiries() {
    const clock = createFakeClock();
    const expired: NextVideoWait[] = [];
    const armings: number[] = [];
    const relocation = createVideoRelocation({ scheduler: clock.scheduler, armFrames: (leaseMs) => void armings.push(leaseMs), onWaitExpired: (wait) => void expired.push(wait) });
    relocation.videoStarted({ role: "film", mayBeAd: false });
    return { clock, relocation, expired, armings };
  }

  it("`feedVideoLost` перед `videoLeft` — ожидание ленты без предела, сторож взведён", () => {
    const { clock, relocation, expired, armings } = relocationWithExpiries();
    assert.equal(relocation.waitsForNextFeedVideo(), false);
    relocation.feedVideoLost();
    assert.equal(relocation.waitsForNextFeedVideo(), true);
    assert.equal(relocation.videoLeft(), "next_in_feed");
    assert.ok(armings.length > 0, "сторож взведён для следующего ролика");
    clock.advance(NEXT_EPISODE_WAIT_MS * 10);
    assert.deepEqual(expired, []);
    assert.equal(relocation.silentClipAdoption(), "accepted", "тихий ролик ленты годится, как после ухода с экрана");
    relocation.stop();
  });

  it("без `feedVideoLost` та же потеря — прежнее короткое ожидание 15 с", () => {
    const { clock, relocation, expired } = relocationWithExpiries();
    assert.equal(relocation.videoLeft(), "next_video");
    clock.advance(NEXT_VIDEO_WAIT_MS);
    assert.deepEqual(expired, ["next_video"]);
  });

  it("новое видео снимает признак ленты: следующее ожидание снова по прежним правилам", () => {
    const { relocation } = relocationWithExpiries();
    relocation.feedVideoLost();
    relocation.videoStarted({ role: "film", mayBeAd: false });
    assert.equal(relocation.waitsForNextFeedVideo(), false);
    assert.equal(relocation.adoptionOpen(), false, "ролик на экране и не доиграл: переезд закрыт");
    relocation.stop();
  });
});
