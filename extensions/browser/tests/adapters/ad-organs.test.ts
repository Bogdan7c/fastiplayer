// Читатели органов рекламы по фикстурам DOM из фактов разведки 09d: что они говорят плееру и что нажимают.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createImaOrgans } from "../../src/adapters/ad-organs/ima.ts";
import { createAllAdOrgans } from "../../src/adapters/ad-organs/index.ts";
import { createPlayerJsOrgans } from "../../src/adapters/ad-organs/playerjs.ts";
import { createYouTubeOrgans } from "../../src/adapters/ad-organs/youtube.ts";
import type { PageAdState } from "../../src/protocol/index.ts";
import { parsePageAdState } from "../../src/shared/ad-watch-validation.ts";
import { createFakeAdView, FakeNode, imaPage, OFFSCREEN_RECT, playerJsPage, youTubePage } from "./fake-ad-dom.ts";

type ShowingAd = Extract<PageAdState, { kind: "showing" }>;
const showing = (pod: ShowingAd["pod"], skip: ShowingAd["skip"]): PageAdState => ({ kind: "showing", pod, skip });
const UNKNOWN_POD = { kind: "unknown" } as const;
const UNAVAILABLE = { kind: "unavailable" } as const;

/** Что бы ни прочитал читатель, это обязано пройти строгий разбор протокола: иначе сеанс умер бы. */
function assertWireSafe(state: PageAdState): void {
  assert.deepEqual(parsePageAdState(state), state);
}

describe("YouTube", () => {
  const read = (page: ReturnType<typeof youTubePage>) => createYouTubeOrgans(createFakeAdView(page.root, { hostname: "www.youtube.com" }));

  it("без класса ad-showing рекламы нет, даже если кнопка лежит в DOM", () => {
    assert.deepEqual(read(youTubePage({ adShowing: false, counterText: "1 из 2", skip: "shown" })).readAdState(), { kind: "none" });
  });

  it("счётчик читается числами в любом языке интерфейса", () => {
    for (const text of ["1 из 2", "Ad 1 of 2", "1 / 2", "広告 1/2"]) {
      const state = read(youTubePage({ adShowing: true, counterText: text, skip: "none" })).readAdState();
      assert.deepEqual(state, showing({ kind: "known", index: 1, count: 2 }, UNAVAILABLE), text);
    }
  });

  it("скрытая кнопка — unavailable, показанная — on_page и никогда available", () => {
    assert.deepEqual(read(youTubePage({ adShowing: true, skip: "hidden" })).readAdState(), showing(UNKNOWN_POD, UNAVAILABLE));
    assert.deepEqual(read(youTubePage({ adShowing: true, skip: "shown" })).readAdState(), showing(UNKNOWN_POD, { kind: "on_page" }));
    // Непропускаемая реклама: кнопки в DOM нет вовсе.
    assert.deepEqual(read(youTubePage({ adShowing: true, skip: "none" })).readAdState(), showing(UNKNOWN_POD, UNAVAILABLE));
  });

  it("pressSkip на YouTube ничего не нажимает: показанная кнопка — person_only, иначе no_button", () => {
    const shown = youTubePage({ adShowing: true, skip: "shown" });
    assert.equal(read(shown).pressSkip(), "person_only");
    assert.equal(shown.skipButton?.clicks, 0);
    assert.equal(read(youTubePage({ adShowing: true, skip: "hidden" })).pressSkip(), "no_button");
    assert.equal(read(youTubePage({ adShowing: false, skip: "shown" })).pressSkip(), "no_button");
  });

  it("мусор в счётчике даёт unknown, а не ошибку и не лишние числа на провод", () => {
    const garbage = ["", "Ad 3 of 2", "0 of 2", "1 of 65", "999999999 of 1000000000", "1 of 2 of 3", "x".repeat(200) + " 1 of 2", "Ad one of two"];
    for (const text of garbage) {
      const state = read(youTubePage({ adShowing: true, counterText: text, skip: "none" })).readAdState();
      assert.deepEqual(state, showing(UNKNOWN_POD, UNAVAILABLE), JSON.stringify(text));
      assertWireSafe(state);
    }
  });

  it("подмена текста страницей не просачивается: наружу идут только числа", () => {
    const state = read(youTubePage({ adShowing: true, counterText: "Ad 1 of 2 — <b>click here</b>", skip: "none" })).readAdState();
    assert.equal(JSON.stringify(state).includes("click"), false);
    assert.deepEqual(state, showing({ kind: "known", index: 1, count: 2 }, UNAVAILABLE));
  });

  it("matches: только хосты YouTube, а не похожие имена", () => {
    const organs = createYouTubeOrgans(createFakeAdView(new FakeNode("html")));
    const place = (hostname: string) => ({ hostname, contains: () => false });
    assert.equal(organs.matches(place("www.youtube.com")), true);
    assert.equal(organs.matches(place("www.youtube-nocookie.com")), true);
    assert.equal(organs.matches(place("notyoutube.com")), false);
    assert.equal(organs.matches(place("youtube.com.evil.example")), false);
  });
});

describe("IMA", () => {
  const read = (page: ReturnType<typeof imaPage>) => createImaOrgans(createFakeAdView(page.root, { hostname: "imasdk.googleapis.com" }));

  it("нет контейнера — рекламы нет", () => {
    assert.deepEqual(createImaOrgans(createFakeAdView(new FakeNode("html"))).readAdState(), { kind: "none" });
  });

  it("отсчёт — число секунд, счётчика N из M нет", () => {
    assert.deepEqual(read(imaPage({ countdownText: "5", skip: "hidden" })).readAdState(), showing(UNKNOWN_POD, { kind: "countdown", seconds: 5 }));
  });

  it("показанная кнопка — available", () => {
    assert.deepEqual(read(imaPage({ countdownText: "1", skip: "shown" })).readAdState(), showing(UNKNOWN_POD, { kind: "available" }));
  });

  it("нет ни отсчёта, ни кнопки — unavailable (непропускаемая)", () => {
    assert.deepEqual(read(imaPage({ skip: "none" })).readAdState(), showing(UNKNOWN_POD, UNAVAILABLE));
  });

  it("мусор в отсчёте — unavailable: числа вне лимита протокола не уходят", () => {
    for (const text of ["", "99999", "5 6", "soon", "-3x7y9", "1".repeat(40)]) {
      const state = read(imaPage({ countdownText: text, skip: "hidden" })).readAdState();
      assert.deepEqual(state, showing(UNKNOWN_POD, UNAVAILABLE), JSON.stringify(text));
      assertWireSafe(state);
    }
  });

  it("pressSkip нажимает именно кнопку пропуска, ничего другого", () => {
    const page = imaPage({ countdownText: "1", skip: "shown" });
    assert.equal(read(page).pressSkip(), "pressed");
    assert.equal(page.skipButton?.clicks, 1);
    assert.equal(page.container.clicks, 0);
    assert.equal(page.countdown?.clicks, 0);
  });

  it("pressSkip при скрытой кнопке — no_button, клика нет", () => {
    const page = imaPage({ countdownText: "3", skip: "hidden" });
    assert.equal(read(page).pressSkip(), "no_button");
    assert.equal(page.skipButton?.clicks, 0);
  });

  it("matches: хост iframe IMA или готовый контейнер в документе", () => {
    const organs = createImaOrgans(createFakeAdView(new FakeNode("html")));
    assert.equal(organs.matches({ hostname: "imasdk.googleapis.com", contains: () => false }), true);
    assert.equal(organs.matches({ hostname: "example.com", contains: (selector) => selector === ".videoAdUi" }), true);
    assert.equal(organs.matches({ hostname: "example.com", contains: () => false }), false);
  });
});

describe("PlayerJS", () => {
  const read = (page: ReturnType<typeof playerJsPage>) => createPlayerJsOrgans(createFakeAdView(page.root));

  it("счётчик и отсчёт на трёх языках", () => {
    const cases: [string, string, PageAdState][] = [
      ["Реклама 1/2", "Пропустити через 15", showing({ kind: "known", index: 1, count: 2 }, { kind: "countdown", seconds: 15 })],
      ["Реклама 2/3", "Пропустить через 7", showing({ kind: "known", index: 2, count: 3 }, { kind: "countdown", seconds: 7 })],
      ["Ad 1/1", "Skip in 3", showing({ kind: "known", index: 1, count: 1 }, { kind: "countdown", seconds: 3 })],
    ];
    for (const [counter, countdown, expected] of cases) {
      assert.deepEqual(read(playerJsPage([{ text: counter }, { text: countdown }])).readAdState(), expected, counter);
    }
  });

  it("на второй рекламе сайт пишет 1/2 — показываем числа сайта", () => {
    const state = read(playerJsPage([{ text: "Реклама 1/2" }, { text: "Пропустити" }])).readAdState();
    assert.deepEqual(state, showing({ kind: "known", index: 1, count: 2 }, { kind: "available" }));
  });

  it("Tortuga: слово «Реклама» без номера — отсчёт кончился, кнопка доступна и нажимается", () => {
    const page = playerJsPage([{ text: "Реклама" }, { text: "Пропустити" }]);
    assert.deepEqual(read(page).readAdState(), showing({ kind: "unknown" }, { kind: "available" }));
    assert.equal(read(page).pressSkip(), "pressed");
    assert.equal(page.leaves[1]?.clicks, 1);
  });

  it("одна фраза со словом реклама внутри счётчиком не считается", () => {
    const page = playerJsPage([{ text: "Вимкнути рекламу" }, { text: "Пропустити" }]);
    assert.deepEqual(read(page).readAdState(), { kind: "none" });
    assert.equal(read(page).pressSkip(), "no_button");
    assert.equal(page.leaves[1]?.clicks, 0);
  });

  it("листья меню за экраном не считаются, даже если их текст похож на кнопку", () => {
    const page = playerJsPage([{ text: "Реклама 1/2" }, { text: "Пропустити", rect: OFFSCREEN_RECT }, { text: "Субтитри", rect: OFFSCREEN_RECT }]);
    assert.deepEqual(read(page).readAdState(), showing({ kind: "known", index: 1, count: 2 }, UNAVAILABLE));
    assert.equal(read(page).pressSkip(), "no_button");
    assert.equal(page.leaves[1]?.clicks, 0);
  });

  it("реклама только за экраном — рекламы нет", () => {
    const page = playerJsPage([{ text: "Реклама 1/2", rect: OFFSCREEN_RECT }, { text: "Пропустити через 5", rect: OFFSCREEN_RECT }]);
    assert.deepEqual(read(page).readAdState(), { kind: "none" });
  });

  it("скрытый лист (display:none) не считается", () => {
    assert.deepEqual(read(playerJsPage([{ text: "Реклама 1/2", rendered: false }])).readAdState(), { kind: "none" });
  });

  it("одинокое «Пропустити» без счётчика и отсчёта рекламой не считается и не нажимается", () => {
    const page = playerJsPage([{ text: "Пропустити" }]);
    assert.deepEqual(read(page).readAdState(), { kind: "none" });
    assert.equal(read(page).pressSkip(), "no_button");
    assert.equal(page.leaves[0]?.clicks, 0);
  });

  it("числа вне лимитов и номер больше размера — pod unknown, реклама всё равно есть", () => {
    for (const counter of ["Реклама 3/2", "Реклама 0/2", "Реклама 1/65", "Реклама 1/999999999"]) {
      const state = read(playerJsPage([{ text: counter }])).readAdState();
      assert.deepEqual(state, showing(UNKNOWN_POD, UNAVAILABLE), counter);
      assertWireSafe(state);
    }
  });

  it("отсчёт вне лимита протокола — unavailable", () => {
    const state = read(playerJsPage([{ text: "Реклама 1/2" }, { text: "Пропустити через 99999" }])).readAdState();
    assert.deepEqual(state, showing({ kind: "known", index: 1, count: 2 }, UNAVAILABLE));
    assertWireSafe(state);
  });

  it("чужой текст рядом не подделывает рекламу", () => {
    const page = playerJsPage([{ text: "Реклама 1/2 купить сейчас" }, { text: "Рекламная пауза" }, { text: "Skip intro" }]);
    assert.deepEqual(read(page).readAdState(), { kind: "none" });
  });

  it("pressSkip нажимает именно лист «Пропустити», а не соседние", () => {
    const page = playerJsPage([{ text: "Реклама 1/2" }, { text: "Швидкість" }, { text: "Пропустити" }]);
    assert.equal(read(page).pressSkip(), "pressed");
    assert.deepEqual(page.leaves.map((leaf) => leaf.clicks), [0, 0, 1]);
  });

  it("слова трёх языков для кнопки", () => {
    for (const word of ["Пропустити", "Пропустить", "Skip", "Skip ad", "ПРОПУСТИТИ"]) {
      const page = playerJsPage([{ text: "Ad 1/2" }, { text: word }]);
      assert.equal(read(page).pressSkip(), "pressed", word);
    }
  });

  it("matches: только документ со своим корнем pjsdiv", () => {
    const organs = createPlayerJsOrgans(createFakeAdView(new FakeNode("html")));
    assert.equal(organs.matches({ hostname: "ashdi.vip", contains: (selector) => selector === "pjsdiv" }), true);
    assert.equal(organs.matches({ hostname: "ashdi.vip", contains: () => false }), false);
  });
});

describe("все читатели вместе", () => {
  it("в обычной странице никто не выдаёт рекламу", () => {
    const organs = createAllAdOrgans(createFakeAdView(new FakeNode("html").add(new FakeNode("div", { text: "Ad 1 of 2" }))));
    assert.deepEqual(organs.map((reader) => reader.readAdState()), [{ kind: "none" }, { kind: "none" }, { kind: "none" }]);
  });
});
