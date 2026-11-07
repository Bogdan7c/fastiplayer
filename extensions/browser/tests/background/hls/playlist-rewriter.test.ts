// Переписчик плейлистов: номерки вместо адресов, приватность, переменные, честные отказы.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createHandleDesk, type HandleDesk, type ResourceKind } from "../../../src/background/handoff/hls/handle-desk.ts";
import type { FileAddressRule } from "../../../src/background/handoff/file-address-guard.ts";
import { admitsResourceUrl } from "../../../src/background/handoff/hls/resource-admission.ts";
import { rewritePlaylist } from "../../../src/background/handoff/hls/playlist-rewriter.ts";
import { REFUSED_RESOURCE_HANDLE } from "../../../src/background/handoff/hls/limits.ts";
import { formatResourceAddress } from "../../../src/protocol/index.ts";

const RULE: FileAddressRule = { pageHost: "video.example.com", setting: "block", consentedHost: null };
const BASE = "https://cdn.example.net/live/master.m3u8";
const REFUSED_ADDRESS = formatResourceAddress(REFUSED_RESOURCE_HANDLE);

/** Переписчик с настоящим допуском и настоящей стойкой - как в продукте. */
function rewrite(text: string, base = BASE): { result: ReturnType<typeof rewritePlaylist>; desk: HandleDesk; kinds: Map<string, ResourceKind> } {
  const desk = createHandleDesk();
  const kinds = new Map<string, ResourceKind>();
  const result = rewritePlaylist(text, base, (url, kind) => {
    if (!admitsResourceUrl(url, RULE)) return undefined;
    kinds.set(url, kind);
    return desk.issue(url, kind);
  });
  return { result, desk, kinds };
}

function textOf(result: ReturnType<typeof rewritePlaylist>): string {
  assert.ok(result.ok, `ожидали успех, получили ${JSON.stringify(result)}`);
  return result.text;
}

describe("мастер-плейлист", () => {
  const master = [
    "#EXTM3U",
    "#EXT-X-VERSION:6",
    '#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",NAME="English",DEFAULT=YES,URI="audio/en.m3u8"',
    '#EXT-X-STREAM-INF:BANDWIDTH=800000,CODECS="avc1.4d401f,mp4a.40.2",AUDIO="aud"',
    "low/index.m3u8",
    "#EXT-X-STREAM-INF:BANDWIDTH=2000000,AUDIO=\"aud\"",
    "https://other.example.org/high/index.m3u8",
    "",
  ].join("\n");

  it("два варианта и аудиодорожка становятся плейлистами с номерками, остальное цело", () => {
    const { result, desk, kinds } = rewrite(master);
    assert.equal(
      textOf(result),
      [
        "#EXTM3U",
        "#EXT-X-VERSION:6",
        `#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",NAME="English",DEFAULT=YES,URI="${formatResourceAddress(0)}"`,
        '#EXT-X-STREAM-INF:BANDWIDTH=800000,CODECS="avc1.4d401f,mp4a.40.2",AUDIO="aud"',
        formatResourceAddress(1),
        '#EXT-X-STREAM-INF:BANDWIDTH=2000000,AUDIO="aud"',
        formatResourceAddress(2),
        "",
      ].join("\n"),
    );
    assert.equal(desk.lookup(0)?.url, "https://cdn.example.net/live/audio/en.m3u8");
    assert.equal(desk.lookup(1)?.url, "https://cdn.example.net/live/low/index.m3u8");
    assert.equal(desk.lookup(2)?.url, "https://other.example.org/high/index.m3u8");
    assert.deepEqual([...kinds.values()], ["playlist", "playlist", "playlist"]);
  });

  it("SESSION-KEY - медиа, I-FRAME-STREAM-INF - плейлист, SESSION-DATA выброшен (не в белом списке)", () => {
    const { result, kinds } = rewrite(
      ["#EXTM3U", '#EXT-X-SESSION-KEY:METHOD=AES-128,URI="key.bin"', '#EXT-X-SESSION-DATA:DATA-ID="com.x",URI="d.json"', '#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=1,URI="iframes.m3u8"', ""].join("\n"),
    );
    const output = textOf(result);
    assert.equal(kinds.get("https://cdn.example.net/live/key.bin"), "media");
    assert.equal(kinds.get("https://cdn.example.net/live/iframes.m3u8"), "playlist");
    assert.ok(!output.includes("SESSION-DATA") && !output.includes("d.json"));
    assert.equal(kinds.has("https://cdn.example.net/live/d.json"), false, "под SESSION-DATA номерок не выдан");
  });

  it("EXT-X-MEDIA-SEQUENCE не принимают за EXT-X-MEDIA: плейлист остаётся медиа-плейлистом", () => {
    const { kinds } = rewrite("#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:10\n#EXTINF:4.0,\nseg10.ts\n");
    assert.equal(kinds.get("https://cdn.example.net/live/seg10.ts"), "media");
  });
});

describe("медиа-плейлист", () => {
  it("ключ AES-128, MAP и сегменты - медиа; BYTERANGE не тронут, у EXTINF осталась длительность", () => {
    const source = [
      "#EXTM3U",
      "#EXT-X-TARGETDURATION:6",
      '#EXT-X-KEY:METHOD=AES-128,URI="https://keys.example.net/k?id=7",IV=0x00000000000000000000000000000001',
      '#EXT-X-MAP:URI="init.mp4",BYTERANGE="720@0"',
      "#EXTINF:5.005,Название, с запятой",
      "#EXT-X-BYTERANGE:1000@720",
      "main.mp4",
      "#EXTINF:5.005,",
      "#EXT-X-BYTERANGE:1000",
      "main.mp4",
      "#EXT-X-ENDLIST",
      "",
    ].join("\n");
    const { result, desk } = rewrite(source);
    assert.equal(
      textOf(result),
      [
        "#EXTM3U",
        "#EXT-X-TARGETDURATION:6",
        `#EXT-X-KEY:METHOD=AES-128,URI="${formatResourceAddress(0)}",IV=0x00000000000000000000000000000001`,
        `#EXT-X-MAP:URI="${formatResourceAddress(1)}",BYTERANGE="720@0"`,
        "#EXTINF:5.005,",
        "#EXT-X-BYTERANGE:1000@720",
        formatResourceAddress(2),
        "#EXTINF:5.005,",
        "#EXT-X-BYTERANGE:1000",
        formatResourceAddress(2),
        "#EXT-X-ENDLIST",
        "",
      ].join("\n"),
    );
    assert.equal(desk.size(), 3, "один и тот же сегмент main.mp4 - один номерок");
    assert.equal(desk.lookup(0)?.url, "https://keys.example.net/k?id=7");
  });

  it("LL-HLS: PART и PRELOAD-HINT - медиа, RENDITION-REPORT - плейлист", () => {
    const { result, kinds } = rewrite(
      [
        "#EXTM3U",
        '#EXT-X-PART:DURATION=0.33,URI="part1.mp4",INDEPENDENT=YES',
        '#EXT-X-PRELOAD-HINT:TYPE=PART,URI="part2.mp4"',
        '#EXT-X-RENDITION-REPORT:URI="../other/live.m3u8",LAST-MSN=100,LAST-PART=2',
        "",
      ].join("\n"),
    );
    assert.ok(result.ok);
    assert.equal(kinds.get("https://cdn.example.net/live/part1.mp4"), "media");
    assert.equal(kinds.get("https://cdn.example.net/live/part2.mp4"), "media");
    assert.equal(kinds.get("https://cdn.example.net/other/live.m3u8"), "playlist");
    assert.ok(result.text.includes("LAST-MSN=100,LAST-PART=2"));
  });

  it("относительные адреса считаются от конечного адреса после перенаправления", () => {
    const { desk } = rewrite("#EXTM3U\n#EXTINF:1,\nseg.ts\n#EXTINF:1,\n/abs/seg2.ts\n#EXTINF:1,\n../up/seg3.ts\n", "https://redirected.example.org/a/b/live.m3u8?token=1");
    assert.equal(desk.lookup(0)?.url, "https://redirected.example.org/a/b/seg.ts");
    assert.equal(desk.lookup(1)?.url, "https://redirected.example.org/abs/seg2.ts");
    assert.equal(desk.lookup(2)?.url, "https://redirected.example.org/a/up/seg3.ts");
  });
});

describe("приватность", () => {
  it("название сегмента в EXTINF выброшено: от тега остаётся только длительность", () => {
    const { result } = rewrite(["#EXTM3U", "#EXTINF:6.006,https://cdn.example.net/secret-title", "s.ts", ""].join("\n"));
    const text = textOf(result);
    assert.equal(text, `#EXTM3U\n#EXTINF:6.006,\n${formatResourceAddress(0)}\n`);
    assert.ok(!text.includes("cdn.example.net"));
  });

  it("абсолютный адрес в атрибуте без кавычек тоже не доходит до плеера: строка выброшена", () => {
    const { result } = rewrite(["#EXTM3U", "#EXT-X-DATERANGE:ID=\"a\",X-LINK=https://ads.example.net/x", "#EXTINF:1,", "s.ts", ""].join("\n"));
    const text = textOf(result);
    assert.ok(!text.includes("ads.example.net"), text);
    assert.ok(!text.includes("DATERANGE"), text);
  });

  it("комментарии и CONTENT-STEERING выброшены", () => {
    const { result } = rewrite(
      ["#EXTM3U", "# секрет https://cdn.example.net/private", '#EXT-X-CONTENT-STEERING:SERVER-URI="https://steer.example.net/s",PATHWAY-ID="a"', "#EXTINF:1,", "s.ts", ""].join("\n"),
    );
    assert.equal(textOf(result), `#EXTM3U\n#EXTINF:1,\n${formatResourceAddress(0)}\n`);
  });

  it("DATERANGE выброшен целиком (не в белом списке): ни с адресом, ни без", () => {
    const { result } = rewrite(
      [
        "#EXTM3U",
        '#EXT-X-DATERANGE:ID="ad1",START-DATE="2024-01-01T00:00:00Z",X-ASSET-URI="https://ads.example.net/a.m3u8"',
        '#EXT-X-DATERANGE:ID="chap",START-DATE="2024-01-01T00:00:00Z",DURATION=10',
        "",
      ].join("\n"),
    );
    assert.equal(textOf(result), "#EXTM3U\n");
  });

  it("в выводе нет ни одного исходного хоста", () => {
    const { result } = rewrite(
      ["#EXTM3U", '#EXT-X-KEY:METHOD=AES-128,URI="https://keys.secret-host.net/k"', "#EXTINF:1,", "https://seg.secret-cdn.org/1.ts", ""].join("\n"),
    );
    const output = textOf(result);
    assert.ok(!output.includes("secret"), output);
    assert.ok(!output.includes("://") || [...output.matchAll(/https:\/\/[^/"\s]+/g)].every((match) => match[0] === "https://resource.fastiplayer-bridge.invalid"));
  });

  it("адрес-заглушка от страницы получает номерок «отказ», а не номерок 0", () => {
    const { result, desk } = rewrite("#EXTM3U\n#EXTINF:1,\nhttps://resource.fastiplayer-bridge.invalid/0\n#EXTINF:1,\nreal.ts\n");
    assert.equal(textOf(result), `#EXTM3U\n#EXTINF:1,\n${REFUSED_ADDRESS}\n#EXTINF:1,\n${formatResourceAddress(0)}\n`);
    assert.equal(desk.lookup(0)?.url, "https://cdn.example.net/live/real.ts");
  });

  it("локальный адрес, data: и skd: заменены номерком «отказ»", () => {
    const { result, desk } = rewrite(
      ["#EXTM3U", '#EXT-X-KEY:METHOD=SAMPLE-AES,URI="skd://abc",KEYFORMAT="com.apple.streamingkeydelivery"', "#EXTINF:1,", "http://192.168.0.7/x.ts", "#EXTINF:1,", "data:video/mp2t;base64,AAAA", ""].join("\n"),
    );
    const output = textOf(result);
    assert.equal(output.split(REFUSED_ADDRESS).length - 1, 3);
    assert.equal(desk.size(), 0);
  });
});

describe("EXT-X-DEFINE", () => {
  it("NAME/VALUE и QUERYPARAM подставляются, строки DEFINE убраны", () => {
    const { result, desk } = rewrite(
      [
        "#EXTM3U",
        '#EXT-X-DEFINE:NAME="host",VALUE="media.example.net"',
        '#EXT-X-DEFINE:QUERYPARAM="token"',
        '#EXT-X-KEY:METHOD=AES-128,URI="https://{$host}/key?t={$token}"',
        "#EXTINF:1,",
        "https://{$host}/seg.ts?t={$token}",
        "",
      ].join("\n"),
      "https://cdn.example.net/live/pl.m3u8?token=a%20b",
    );
    const output = textOf(result);
    assert.ok(!output.includes("DEFINE"));
    assert.equal(desk.lookup(0)?.url, "https://media.example.net/key?t=a%20b");
    assert.equal(desk.lookup(1)?.url, "https://media.example.net/seg.ts?t=a%20b");
  });

  it("IMPORT, необъявленная переменная, повтор имени и QUERYPARAM без значения - отказ", () => {
    assert.deepEqual(rewrite('#EXTM3U\n#EXT-X-DEFINE:IMPORT="x"\n').result, { ok: false, reason: "define_import_unsupported" });
    assert.deepEqual(rewrite("#EXTM3U\n#EXTINF:1,\n{$nope}/s.ts\n").result, { ok: false, reason: "undefined_variable" });
    assert.deepEqual(rewrite('#EXTM3U\n#EXT-X-DEFINE:NAME="a",VALUE="1"\n#EXT-X-DEFINE:NAME="a",VALUE="2"\n').result, { ok: false, reason: "invalid_define" });
    assert.deepEqual(rewrite('#EXTM3U\n#EXT-X-DEFINE:QUERYPARAM="absent"\n').result, { ok: false, reason: "invalid_define" });
  });
});

describe("форма текста", () => {
  it("CRLF, одиночный CR и BOM принимаются, вывод с \\n", () => {
    const { result } = rewrite("﻿#EXTM3U\r\n#EXTINF:1,\r\na.ts\r#EXT-X-ENDLIST\r\n");
    assert.equal(textOf(result), `#EXTM3U\n#EXTINF:1,\n${formatResourceAddress(0)}\n#EXT-X-ENDLIST\n`);
  });

  it("битый EXTINF или BYTERANGE - отказ плейлисту, а не сегмент без длительности или диапазона", () => {
    for (const broken of ["#EXTINF:abc,", "#EXTINF", "#EXT-X-BYTERANGE:12x", "#EXT-X-BYTERANGE"]) {
      const { result, desk } = rewrite(["#EXTM3U", "#EXT-X-TARGETDURATION:6", broken, "s.ts", ""].join("\n"));
      assert.deepEqual(result, { ok: false, reason: "malformed_segment_tag" }, broken);
      assert.equal(desk.size(), 0, `${broken}: номерок не выдан`);
    }
  });

  it("не плейлист - not_a_playlist; порча атрибутов с URI - отказ", () => {
    assert.deepEqual(rewrite("<html>https://x.example/a</html>").result, { ok: false, reason: "not_a_playlist" });
    assert.deepEqual(rewrite('#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI="oops\n').result, { ok: false, reason: "malformed_attributes" });
    assert.deepEqual(rewrite("#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=unquoted.bin\n").result, { ok: false, reason: "malformed_attributes" });
  });
});
