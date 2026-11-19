// Сторож следующего видео: `play()` страницы, сказанный ДО взвода (16b3b, стенд `feed-back-recreated-pool`).
//
// Строка стенда (Chromium): пул TikTok на шаге назад даёт заготовленному элементу новый MediaSource, ставит наш
// ролик на паузу и той же задачей зовёт `play()` заготовленного. Взвод фона ещё в пути (фон взводит сторож, только
// узнав об уходе нашего ролика), поэтому `play()` уходит браузеру мимо сторожа, и обещание у страницы — настоящее.
// Данных ещё нет: обещание ждёт. Взвод приходит, приходят метаданные — и тень, вставая, ставила настоящий элемент
// на паузу: браузер отвергал ждущее обещание страницы `AbortError: The play() request was interrupted by a call to
// pause()`, которого страница не вызывала. Здесь НАСТОЯЩАЯ тень на фейковом DOM, который, как браузер, отвергает
// ждущие обещания `play()` при `pause()`.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { flushMicrotasks } from "./shadow/fake-media-dom.ts";
import { browserPlays, LEASE_MS, pagePlay, setupGuard } from "./support/next-video-guard-setup.ts";

/** Заготовленный элемент пула: новый MediaSource уже на нём, данных ещё нет. */
function poolPage() {
  const page = setupGuard();
  const fresh = page.addVideo((video) => {
    video.real.readyState = 0;
  });
  return { ...page, fresh };
}

/** Чем кончилось обещание `play()` страницы (`pending` — ещё ждёт). */
function watchOutcome(promise: Promise<void>): { value: string } {
  const outcome = { value: "pending" };
  promise.then(
    () => (outcome.value = "resolved"),
    (error: { name?: string }) => (outcome.value = `rejected:${error.name ?? String(error)}`),
  );
  return outcome;
}

function heldReports(sent: ReturnType<typeof setupGuard>["sent"]): number {
  return sent.filter((event) => event.kind === "next_video_held").length;
}

describe("play() страницы ушёл браузеру до взвода: тень не отвергает его обещание", () => {
  it("взвод, потом метаданные: тень ждёт, пока браузер забрал обещание на исполнение, и встаёт на `playing`", async () => {
    const { guard, sent, fresh } = poolPage();
    const outcome = watchOutcome(pagePlay(fresh));
    assert.equal(browserPlays(fresh), 1, "сторож не взведён: play() ушёл браузеру");
    guard.arm(LEASE_MS);
    await flushMicrotasks();
    // Метаданные пришли: элемент хочет играть, но настоящее обещание страницы ещё ждёт данных.
    fresh.real.readyState = 1;
    fresh.fireTrusted("loadedmetadata");
    await flushMicrotasks();
    assert.equal(outcome.value, "pending", "обещание страницы не отвергнуто AbortError");
    assert.equal(heldReports(sent), 0, "тень не встала поверх ждущего play() страницы");

    // Данные пришли: браузер забрал обещание на исполнение и прислал `playing` — теперь тень можно ставить.
    fresh.testDataArrived();
    await flushMicrotasks();
    assert.equal(outcome.value, "resolved", "страница дождалась своего play()");
    assert.equal(heldReports(sent), 1, "видео задержано для фона");
    assert.equal(fresh.real.paused, true, "под тенью настоящий элемент стоит");
  });

  it("метаданные пришли раньше взвода: обход при взводе тоже не ставит тень поверх ждущего play()", async () => {
    const { guard, sent, fresh } = poolPage();
    const outcome = watchOutcome(pagePlay(fresh));
    fresh.real.readyState = 1;
    guard.arm(LEASE_MS);
    await flushMicrotasks();
    assert.equal(outcome.value, "pending");
    assert.equal(heldReports(sent), 0);
    fresh.testDataArrived();
    await flushMicrotasks();
    assert.equal(outcome.value, "resolved");
    assert.equal(heldReports(sent), 1);
  });

  it("прежний play() браузер давно исполнил, затем новый источник в том же элементе — play() при взводе задержан на метаданных", async () => {
    // Стенд `ad-film-same-element` и `episodes delayed-next` (регресс b84bfb59): реклама (серия 1) запущена ДО
    // передачи — `play()` мимо сторожа, браузер исполнил его обещание и ролик играл. Потом тот же `<video>` получил
    // фильм (серию 2) и `play()` уже при взводе. Метка «обещание страницы может ждать» осталась от рекламы, и тень
    // не вставала на метаданных фильма: фильм не уходил в окно, а обещание страницы так и не исполнялось.
    const { guard, sent, fresh } = poolPage();
    const adPlay = watchOutcome(pagePlay(fresh));
    fresh.testDataArrived();
    await flushMicrotasks();
    assert.equal(adPlay.value, "resolved", "браузер исполнил play() рекламы");

    // Реклама доиграла: тот же элемент, новый источник — данных снова нет.
    fresh.src = "blob:https://site.example/ms-2";
    guard.arm(LEASE_MS);
    const filmPlay = watchOutcome(pagePlay(fresh));
    fresh.real.readyState = 1;
    fresh.fireTrusted("loadedmetadata");
    await flushMicrotasks();
    assert.equal(heldReports(sent), 1, "фильм задержан на метаданных, как без прежнего play()");
    assert.equal(filmPlay.value, "pending", "обещание фильма у тени, а не отвергнуто");
  });

  it("страховка: play() при взводе по-прежнему задерживается сразу на метаданных, обещание достаётся тени", async () => {
    const { guard, sent, fresh } = poolPage();
    guard.arm(LEASE_MS);
    const outcome = watchOutcome(pagePlay(fresh));
    fresh.real.readyState = 1;
    fresh.fireTrusted("loadedmetadata");
    await flushMicrotasks();
    assert.equal(heldReports(sent), 1, "тень встала на метаданных, как раньше (Ashdi, Tortuga)");
    assert.equal(outcome.value, "pending", "обещание у тени, а не отвергнуто");
  });

  it("страховка: видео, запущенное мимо play() (autoplay), обход при взводе задерживает сразу", () => {
    const { guard, sent, addVideo } = setupGuard();
    const running = addVideo((video) => {
      video.real.paused = false;
      video.real.readyState = 1;
    });
    guard.arm(LEASE_MS);
    assert.equal(heldReports(sent), 1);
    assert.equal(running.real.paused, true);
  });
});
