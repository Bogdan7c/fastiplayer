// «Основной домен» адреса вкладки (сессия 16c, решение 304): таблица случаев правила и сравнение «тот же сайт».
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { baseSiteOfUrl, isSameBaseSite } from "../src/shared/base-site.ts";

describe("основной домен адреса вкладки", () => {
  it("таблица: поддомены, многосоставные суффиксы, хостинги страниц, IP, localhost, одно слово", () => {
    const rows: readonly [string, string | undefined][] = [
      ["https://uaserials.pro/serial/1", "uaserials.pro"],
      ["https://www.uaserials.pro/serial/1", "uaserials.pro"],
      ["https://cdn.player.uaserials.pro/", "uaserials.pro"],
      ["https://WWW.Example.COM./watch", "example.com"],
      ["https://www.bbc.co.uk/iplayer", "bbc.co.uk"],
      ["https://news.bbc.co.uk/", "bbc.co.uk"],
      ["https://rezka.com.ua/film", "rezka.com.ua"],
      ["https://video.site.kyiv.ua/", "site.kyiv.ua"],
      ["https://alice.github.io/page", "alice.github.io"],
      ["https://bob.github.io/page", "bob.github.io"],
      ["http://192.168.0.10:8080/movie", "192.168.0.10"],
      ["http://[::1]:8000/", "[::1]"],
      ["http://localhost:3000/", "localhost"],
      ["http://intranet/video", "intranet"],
      ["https://co.uk/", "co.uk"],
    ];
    for (const [url, site] of rows) assert.equal(baseSiteOfUrl(url), site, url);
  });

  it("не http(s), битый или неизвестный адрес — сайта нет", () => {
    for (const url of ["about:blank", "chrome://newtab/", "file:///home/video.mp4", "not a url", "moz-extension://abc/options.html", undefined]) {
      assert.equal(baseSiteOfUrl(url), undefined, String(url));
    }
  });

  it("тот же сайт: поддомен и путь не важны; чужой сайт, соседи на хостинге и неизвестный адрес — не тот же", () => {
    assert.equal(isSameBaseSite("https://www.example.com/watch?v=1", "https://example.com/watch?v=2"), true);
    assert.equal(isSameBaseSite("https://www.example.com/watch?v=1", "https://other-site.test/"), false);
    assert.equal(isSameBaseSite("https://alice.github.io/", "https://bob.github.io/"), false);
    assert.equal(isSameBaseSite("https://a.bbc.co.uk/", "https://b.bbc.co.uk/"), true);
    assert.equal(isSameBaseSite(undefined, undefined), false, "без адреса сравнивать нечего");
    assert.equal(isSameBaseSite("about:blank", "about:blank"), false);
  });
});
