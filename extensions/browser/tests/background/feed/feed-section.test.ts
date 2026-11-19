// Раздел адреса ленты (16b3, правило F5): шаг ленты меняет только идентификатор ролика, уход — слова пути.
// Адреса — формы живых лент из наблюдения 19 ноября 2026 (Instagram, TikTok, YouTube Shorts).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { feedSectionOf, isIdentifierSegment } from "../../../src/background/handoff/feed/feed-section.ts";

const sameSection = (left: string, right: string): boolean => {
  const leftSection = feedSectionOf(left);
  assert.notEqual(leftSection, undefined, left);
  return leftSection === feedSectionOf(right);
};

describe("раздел адреса: шаг ленты — тот же раздел", () => {
  it("Instagram: открытая лента /reels/ и первый шаг /reels/<id>/ — один раздел (I1)", () => {
    assert.equal(sameSection("https://www.instagram.com/reels/", "https://www.instagram.com/reels/C7xYz1AbCdE/"), true);
    assert.equal(sameSection("https://www.instagram.com/reels/C7xYz1AbCdE/", "https://www.instagram.com/reels/DBq-9_kLmNo/"), true);
  });

  it("Shorts: /shorts/<id> → /shorts/<другой id>, запрос не входит", () => {
    assert.equal(sameSection("https://www.youtube.com/shorts/dQw4w9WgXcQ", "https://www.youtube.com/shorts/aBcDeFgHiJk?feature=share"), true);
  });

  it("TikTok в профиле: /@кто/video/<число> — имя и число подстановки", () => {
    assert.equal(sameSection("https://www.tiktok.com/@someone/video/7350123456789012345", "https://www.tiktok.com/@other.one/video/7350999999999999999"), true);
  });
});

describe("раздел адреса: уход — другой раздел", () => {
  it("Instagram: /reels/<id>/ → /explore/ и → /direct/inbox/ (I6)", () => {
    assert.equal(sameSection("https://www.instagram.com/reels/C7xYz1AbCdE/", "https://www.instagram.com/explore/"), false);
    assert.equal(sameSection("https://www.instagram.com/reels/C7xYz1AbCdE/", "https://www.instagram.com/direct/inbox/"), false);
  });

  it("TikTok: /foryou → /explore (T5); Shorts: /shorts/<id> → главная / (S4)", () => {
    assert.equal(sameSection("https://www.tiktok.com/foryou", "https://www.tiktok.com/explore"), false);
    assert.equal(sameSection("https://www.youtube.com/shorts/dQw4w9WgXcQ", "https://www.youtube.com/"), false);
  });

  it("другой хост — другой раздел, даже с тем же путём", () => {
    assert.equal(sameSection("https://www.instagram.com/reels/", "https://m.instagram.com/reels/"), false);
  });
});

describe("раздел адреса: края", () => {
  it("неразборный адрес — раздела нет (правило F5 о нём молчит)", () => {
    assert.equal(feedSectionOf("not a url"), undefined);
    assert.equal(feedSectionOf(""), undefined);
  });

  it("что считается идентификатором: длинный код, число, @имя; короткое слово — нет", () => {
    assert.equal(isIdentifierSegment("C7xYz1AbCdE"), true);
    assert.equal(isIdentifierSegment("12"), true);
    assert.equal(isIdentifierSegment("@x"), true);
    assert.equal(isIdentifierSegment("reels"), false);
    assert.equal(isIdentifierSegment("explore"), false, "7 знаков — ещё слово");
    assert.equal(isIdentifierSegment("@"), false, "одна собака — не имя");
  });

  it("известный предел: длинное слово пути похоже на идентификатор и выбрасывается", () => {
    // /foryou → /following уход виден (foryou — слово), а /feed/subscriptions → /feed/history — нет.
    assert.equal(sameSection("https://www.tiktok.com/foryou", "https://www.tiktok.com/following"), false);
    assert.equal(sameSection("https://www.youtube.com/feed/subscriptions", "https://www.youtube.com/feed"), true);
  });
});
