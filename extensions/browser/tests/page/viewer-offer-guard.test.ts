// Предложение «Ask» режима 2 (сессия 12b) в MAIN-мире: сторож держит задержанное зрителем видео, пока человек
// выбирает. Настоящая тень на фейковом DOM: проверяем то, что видят страница (пауза/игра модели тени) и браузер
// (играл ли настоящий элемент), и сигналы изолированному миру.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { flushMicrotasks, type FakeVideo } from "./shadow/fake-media-dom.ts";
import { browserPlays, FILE_URL, LEASE_MS, pagePlay, setupGuard } from "./support/next-video-guard-setup.ts";

/** Что видит страница: модель тени (переопределённый `paused`). */
const pageSeesPaused = (video: FakeVideo): boolean => (video as unknown as { paused: boolean }).paused;

/** Охрана включена, свежий клик; фильм-файл задержан запуском зрителя и висит под предложением. */
async function offeredFilm(options: { readonly collectOddities?: boolean } = {}) {
  const page = setupGuard(options);
  page.guard.setViewerPlayWatch("on");
  page.activation.transient = true;
  const film = page.addVideo((video) => {
    video.real.src = FILE_URL;
  });
  void pagePlay(film);
  assert.equal(page.sent.at(-1)?.kind, "next_video_held", "фону ушло донесение");
  page.guard.holdForViewerOffer(page.keyOf(film));
  await flushMicrotasks();
  return { ...page, film };
}

describe("предложение «Ask» в MAIN-мире", () => {
  it("держим: страница видит паузу, браузер видео не запускал, изолированный мир знает элемент", async () => {
    const { film, viewerOffer } = await offeredFilm();
    assert.equal(pageSeesPaused(film), true, "плеер сайта видит обычную паузу");
    assert.equal(film.real.paused, true);
    assert.equal(browserPlays(film), 0, "браузер ничего не декодировал и не озвучивал");
    assert.deepEqual(viewerOffer.stood, [film]);
    assert.equal(viewerOffer.ended, 0);
  });

  it("«Play here» (отпустить): видео играет в браузере, панель уходит; пауза и новый play больше не спрашивают", async () => {
    const { guard, film, keyOf, viewerOffer, sent, harness, activation } = await offeredFilm();
    guard.release(keyOf(film));
    await flushMicrotasks();
    assert.equal(pageSeesPaused(film), false, "страница снова видит игру");
    assert.equal(film.real.paused, false, "играет настоящий элемент");
    assert.equal(browserPlays(film), 1);
    assert.equal(viewerOffer.ended, 1);

    // Решение 257: то же видео на этой странице больше не спрашивается.
    sent.length = 0;
    (film as unknown as { pause(): void }).pause();
    harness.clock.advance(1_000);
    activation.lastInputAtMs = harness.clock.nowMs;
    await pagePlay(film);
    assert.deepEqual(sent, [], "новый play — без донесения фону");
    assert.equal(film.real.paused, false);
  });

  it("«в плеер» (take): модель снова «играет» для плеера, браузер не играл, панель уходит", async () => {
    const { guard, film, viewerOffer } = await offeredFilm();
    const taken = guard.take(film);
    assert.notEqual(taken, null);
    assert.equal(taken?.session.pageSawPlaying(), true, "плеер начнёт с игры, а не с паузы предложения");
    assert.equal(pageSeesPaused(film), false);
    assert.equal(browserPlays(film), 0);
    assert.equal(viewerOffer.ended, 1);
  });

  it("снято (новое предложение на вкладке): видео стоит на паузе, отказ не запомнен — новый play снова спрашивает", async () => {
    const { guard, film, keyOf, viewerOffer, sent, harness, activation } = await offeredFilm();
    guard.withdrawViewerOffer(keyOf(film));
    await flushMicrotasks();
    assert.equal(pageSeesPaused(film), true);
    assert.equal(film.real.paused, true);
    assert.equal(browserPlays(film), 0);
    assert.equal(viewerOffer.ended, 1);
    assert.equal(guard.take(film), null, "видео больше не задержано");

    sent.length = 0;
    harness.clock.advance(1_000);
    activation.lastInputAtMs = harness.clock.nowMs;
    void pagePlay(film);
    assert.equal(sent.at(-1)?.kind, "next_video_held", "человек снова нажал play — снова вопрос");
  });

  it("одно предложение на документ: второе видео под предложением снимает первое (оно на паузе)", async () => {
    const { guard, film, addVideo, keyOf, viewerOffer, harness, activation } = await offeredFilm();
    const second = addVideo((video) => {
      video.real.src = "https://cdn.example/second.mp4";
    });
    harness.clock.advance(500);
    activation.lastInputAtMs = harness.clock.nowMs;
    void pagePlay(second);
    guard.holdForViewerOffer(keyOf(second));
    await flushMicrotasks();
    assert.deepEqual(viewerOffer.stood, [film, second]);
    assert.equal(viewerOffer.ended, 1, "панель первого ушла");
    assert.equal(guard.take(film), null);
    assert.equal(pageSeesPaused(film), true);
    assert.equal(browserPlays(film), 0);
  });

  it("изолированный мир о предложении не узнал: держать без панели нельзя — видео играет здесь, странность записана", async () => {
    const page = setupGuard({ collectOddities: true });
    page.guard.setViewerPlayWatch("on");
    page.activation.transient = true;
    page.viewerOffer.accepts = false;
    const film = page.addVideo((video) => {
      video.real.src = FILE_URL;
    });
    void pagePlay(film);
    page.guard.holdForViewerOffer(page.keyOf(film));
    await flushMicrotasks();
    assert.equal(pageSeesPaused(film), false);
    assert.equal(film.real.paused, false);
    assert.equal(page.oddities.length, 1);
  });

  it("чужой ключ и видео, задержанное не зрителем (переезд сеанса), предложением не становятся", async () => {
    const { guard, addVideo, keyOf, viewerOffer } = setupGuard();
    const banner = addVideo((video) => {
      video.real.src = FILE_URL;
    });
    guard.arm(LEASE_MS);
    void pagePlay(banner);
    guard.holdForViewerOffer(keyOf(banner));
    guard.holdForViewerOffer(9_999);
    assert.deepEqual(viewerOffer.stood, []);
    assert.equal(pageSeesPaused(banner), false, "модель тени не ставилась на паузу");
  });

  it("страница сменила источник под предложением: удержание снято, панель уходит", async () => {
    const { film, guard, viewerOffer } = await offeredFilm();
    (film as unknown as { src: string }).src = "https://cdn.example/other.mp4";
    await flushMicrotasks();
    assert.equal(guard.take(film), null);
    assert.equal(viewerOffer.ended, 1);
  });
});
