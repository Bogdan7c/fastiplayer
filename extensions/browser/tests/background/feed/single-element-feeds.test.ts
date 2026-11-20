// Список лент одного элемента (16b3c, решение 374): какие адреса — лента Shorts, какие — нет, и память сеанса
// «был в ленте / ушёл из неё». Проверяется ответ на конкретные адреса, а не устройство списка.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { feedSectionOf } from "../../../src/background/handoff/feed/feed-section.ts";
import {
  createSingleElementFeedWatch,
  singleElementFeedEntries,
  singleElementFeedPlaceOf,
} from "../../../src/background/handoff/feed/single-element-feeds.ts";

const SHORTS = "https://www.youtube.com/shorts/dQw4w9WgXcQ";
const BENCH_SHORTS = "http://127.0.0.1:41234/shorts/Sh2aBcDeFgHiJ";
const BENCH_PAGE = "http://127.0.0.1:41234/pages/feed-edges.html?run=r-1&shape=moved";

describe("адрес — лента одного элемента?", () => {
  for (const url of [
    SHORTS,
    "https://youtube.com/shorts/dQw4w9WgXcQ?feature=share",
    "https://m.youtube.com/shorts/dQw4w9WgXcQ",
    "https://www.youtube.com/shorts/",
    BENCH_SHORTS,
    "http://localhost:41234/shorts/Sh3aBcDeFgHiJ",
  ]) {
    it(`${url} — да, раздел ленты как у feed-section`, () => {
      assert.deepEqual(singleElementFeedPlaceOf(url), { kind: "single_element_feed", section: feedSectionOf(url) });
    });
  }

  for (const url of [
    "https://www.youtube.com/",
    "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
    "https://www.youtube.com/shortsx/dQw4w9WgXcQ",
    "https://www.youtube.com/@channel/shorts",
    "https://notyoutube.com/shorts/dQw4w9WgXcQ",
    "https://www.instagram.com/reels/C7xYz1AbCdE/",
    "https://serial.example/serial/123/4",
    BENCH_PAGE,
    "http://127.0.0.1:41234/serial/123/4",
    "http://127.0.0.1:41234/",
  ]) {
    it(`${url} — нет (сериал, главная, другой раздел, граница сегмента)`, () => {
      assert.deepEqual(singleElementFeedPlaceOf(url), { kind: "elsewhere" });
    });
  }

  for (const url of ["not a url", "about:blank", "file:///home/shorts/a.mp4", "chrome://newtab/"]) {
    it(`${url} — адрес не разобрать: о ленте не судим`, () => {
      assert.deepEqual(singleElementFeedPlaceOf(url), { kind: "unknown_address" });
    });
  }

  it("стенд в списке только с путём: голый локальный хост сделал бы лентой все страницы стенда", () => {
    const localEntries = singleElementFeedEntries().filter((entry) => entry.startsWith("127.0.0.1") || entry.startsWith("localhost"));
    assert.deepEqual(localEntries, ["127.0.0.1/shorts", "localhost/shorts"]);
  });

  it("список подменяется: адрес судится по переданным записям, встроенные не подмешиваются", () => {
    assert.equal(singleElementFeedPlaceOf(SHORTS, ["example.com/feed"]).kind, "elsewhere");
    assert.equal(singleElementFeedPlaceOf("https://example.com/feed/42", ["example.com/feed"]).kind, "single_element_feed");
    assert.equal(singleElementFeedPlaceOf(SHORTS, []).kind, "elsewhere");
  });
});

describe("память сеанса о ленте одного элемента", () => {
  it("открыт не в ленте и туда не входил — never_in_feed даже после смены адреса (сериал)", () => {
    const watch = createSingleElementFeedWatch("https://serial.example/serial/123/4");
    watch.addressSeen("https://serial.example/serial/123/5");
    watch.addressSeen("https://serial.example/");
    assert.equal(watch.state(), "never_in_feed");
    assert.equal(watch.lastFeedSection(), undefined);
  });

  it("открыт на стенде вне ленты, первый pushState вводит в ленту, уход на главную — left_feed", () => {
    const watch = createSingleElementFeedWatch(BENCH_PAGE);
    assert.equal(watch.state(), "never_in_feed");
    watch.addressSeen(BENCH_SHORTS);
    assert.equal(watch.state(), "in_feed");
    watch.addressSeen("http://127.0.0.1:41234/");
    assert.equal(watch.state(), "left_feed");
    assert.equal(watch.lastFeedSection(), feedSectionOf(BENCH_SHORTS), "раздел для «Назад» — раздел ленты, не адрес открытия");
  });

  it("открыт прямо в Shorts — in_feed сразу; вернулся в ленту — снова in_feed", () => {
    const watch = createSingleElementFeedWatch(SHORTS);
    assert.equal(watch.state(), "in_feed");
    watch.addressSeen("https://www.youtube.com/");
    assert.equal(watch.state(), "left_feed");
    watch.addressSeen("https://www.youtube.com/shorts/aBcDeFgHiJk");
    assert.equal(watch.state(), "in_feed");
  });

  it("неразборный адрес состояние не меняет ни в ленте, ни после ухода", () => {
    const watch = createSingleElementFeedWatch(SHORTS);
    watch.addressSeen("not a url");
    assert.equal(watch.state(), "in_feed");
    watch.addressSeen("https://www.youtube.com/");
    watch.addressSeen("about:blank");
    assert.equal(watch.state(), "left_feed");
  });

  it("адрес открытия неизвестен — never_in_feed, пока браузер не назовёт адрес ленты", () => {
    const watch = createSingleElementFeedWatch(undefined);
    assert.equal(watch.state(), "never_in_feed");
    watch.addressSeen(SHORTS);
    assert.equal(watch.state(), "in_feed");
  });
});
