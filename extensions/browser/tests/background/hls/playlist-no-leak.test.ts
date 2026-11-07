// Переписчик не выпускает чужие адреса: таблица враждебных строк и перебор «имя тега × разделитель ×
// вид значения». Правило одно: на выходе нет ни хоста, ни пути, ни токена (или переписчик честно
// отказал, `ok:false`), а каждая строка вывода - тег по шаблону либо адрес-заглушка.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { FileAddressRule } from "../../../src/background/handoff/file-address-guard.ts";
import { createHandleDesk } from "../../../src/background/handoff/hls/handle-desk.ts";
import { rewritePlaylist } from "../../../src/background/handoff/hls/playlist-rewriter.ts";
import { admitsResourceUrl } from "../../../src/background/handoff/hls/resource-admission.ts";

const RULE: FileAddressRule = { pageHost: "video.example.com", setting: "block", consentedHost: null };
const SECRET_MARKERS = ["secret", "tok3n"] as const;
const BASE = "https://cdn.example.net/live/pl.m3u8?token=secret-tok3n";
/** Допустимая строка вывода: тег по шаблону или адрес-заглушка. */
const OUTPUT_LINE = /^(?:#EXT[A-Z0-9-]*(?::.*)?|https:\/\/resource\.fastiplayer-bridge\.invalid\/\d+)$/;

function rewriteWith(text: string): ReturnType<typeof rewritePlaylist> {
  const desk = createHandleDesk();
  return rewritePlaylist(text, BASE, (url, kind) => (admitsResourceUrl(url, RULE) ? desk.issue(url, kind) : undefined));
}

/** Нет утечки: отказ или вывод без секретов и с чистыми строками. */
function assertNoLeak(text: string, label: string): void {
  const result = rewriteWith(text);
  if (!result.ok) return;
  for (const marker of SECRET_MARKERS) assert.ok(!result.text.includes(marker), `${label}: в выводе «${marker}»:\n${result.text}`);
  for (const line of result.text.trimEnd().split("\n")) assert.match(line, OUTPUT_LINE, `${label}: чужая строка`);
}

const HOSTILE_LINES: readonly string[] = [
  "#EXT-X-FOO https://secret.example/a",
  '#EXT-X-MAP:uri="//secret.example/i.mp4"',
  '#EXT-X-CUSTOM:X="//secret.example/p"',
  '#EXT-X-CUSTOM:"//secret.example"',
  "#EXT-X-CUSTOM:secret.example/path",
  "#EXTINF :6,secret.example/x",
  "#EXTINF:6 secret.example/x",
  "#EXTINF:6.0,https://secret.example/x",
  '#EXT-X-DEFINE :NAME="a",VALUE="secret.example"',
  '#EXT-X-MEDIA:TYPE=AUDIO,NAME="{$a}"',
  "#EXT-X-STREAM-INF :BANDWIDTH=1",
  '#EXT-X-DEFINE:QUERYPARAM="token"',
  '#EXT-X-MEDIA:TYPE=AUDIO,NAME="{$token}",GROUP-ID="g"',
  '#EXT-X-DEFINE:NAME="{$tok}",VALUE="secret.example"',
  '#ext-x-key:METHOD=NONE,URI="https://secret.example/k"',
  '#EXT-x-MAP:URI="https://secret.example/i"',
  '#EXT-X-MAP:URI="x.mp4"\t,BYTERANGE="1@0"',
  "#EXT-X-START:TIME-OFFSET=https://secret.example/a",
  "#EXT-X-START:TIME-OFFSET=secret.example/a",
  "#EXT-X-KEY:METHOD=AES-128,URI=https://secret.example/k",
  '#EXT-X-KEY:METHOD=AES-128,URI="k",IV="//secret.example"',
  '#EXT-X-UNKNOWN:URI="https://secret.example/u"',
  '#EXT-X-DATERANGE:ID="a",X-ASSET-URI="https://secret.example/a"',
  '#EXT-X-SESSION-DATA:DATA-ID="a",VALUE="secret",URI="https://secret.example/d"',
  '#EXT-X-CONTENT-STEERING:SERVER-URI="https://secret.example/s"',
  "# https://secret.example/comment",
  "#EXT-X-PROGRAM-DATE-TIME:https://secret.example",
  "#EXT-X-BYTERANGE:secret.example",
  "#EXT-X-VERSION:3 secret",
  "#EXT-X-ENDLIST secret",
  "#EXT-X-DISCONTINUITY:secret.example",
  "#EXT-X-TARGETDURATION:6;secret",
  "#EXT-X-PLAYLIST-TYPE:secret",
  '#EXT-X-PART:DURATION=1,URI="p.mp4",NAME="\\\\secret"',
  '#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS="secret?x=1"',
  '#EXT-X-SKIP:SKIPPED-SEGMENTS=1,RECENTLY-REMOVED-DATERANGES="secret.example/a"',
  '#EXT-X-MEDIA:TYPE=AUDIO,NAME="x",secret.example/a',
  "#EXT-X-MEDIA:TYPE=AUDIO,NAME=secret.example",
];

describe("враждебные строки не выпускают адрес", () => {
  for (const line of HOSTILE_LINES) {
    it(JSON.stringify(line), () => {
      assertNoLeak(`#EXTM3U\n${line}\n#EXTINF:1,\nok.ts\n`, "медиа");
      assertNoLeak(`#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nlow.m3u8\n${line}\n`, "мастер");
      assertNoLeak(`#EXTM3U\r${line}\r#EXTINF:1,\rok.ts\r`, "CR");
      assertNoLeak(`﻿  \n#EXTM3U\r\n${line}\r\n`, "BOM и пробелы");
    });
  }

  it("вторая метка #EXTM3U с адресом и первая строка с адресом", () => {
    assertNoLeak("#EXTM3U\n#EXTM3U https://secret.example/a\nx.ts\n", "вторая метка");
    assert.deepEqual(rewriteWith("#EXTM3U https://secret.example/a\n#EXTINF:1,\nx.ts\n"), { ok: false, reason: "not_a_playlist" });
    assert.deepEqual(rewriteWith("junk\n#EXTM3U\n"), { ok: false, reason: "not_a_playlist" });
  });

  it("токен из адреса плейлиста не попадает в атрибуты, а в адрес попадает (но не в вывод)", () => {
    const result = rewriteWith('#EXTM3U\n#EXT-X-DEFINE:QUERYPARAM="token"\n#EXT-X-MEDIA:TYPE=AUDIO,NAME="{$token}"\n#EXTINF:1,\ns.ts?t={$token}\n');
    assert.ok(result.ok);
    assert.ok(!result.text.includes("tok3n"));
    assert.ok(!result.text.includes("NAME"), "строка с {$token} вне URI выброшена");
  });

  it("KEY, SESSION-KEY и MAP с нарушением - отказ плейлисту, а не молчаливый выброс", () => {
    for (const line of ["#EXT-X-KEY:METHOD=AES-128,URI=unquoted", '#EXT-X-MAP:uri="x"', '#EXT-X-SESSION-KEY:METHOD=AES-128,URI="k",KEYFORMAT="//secret"']) {
      assert.deepEqual(rewriteWith(`#EXTM3U\n${line}\n`), { ok: false, reason: "malformed_attributes" }, line);
    }
  });

  it("законные значения со слэшем (KEYFORMATVERSIONS) проходят", () => {
    const result = rewriteWith('#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI="k",KEYFORMAT="identity",KEYFORMATVERSIONS="1/2/5"\n');
    assert.ok(result.ok);
    assert.ok(result.text.includes('KEYFORMATVERSIONS="1/2/5"'));
  });
});

describe("перебор: имя тега x разделитель x вид значения", () => {
  const names = ["#EXT-X-KEY", "#EXT-X-MAP", "#EXT-X-MEDIA", "#EXT-X-STREAM-INF", "#EXT-X-PART", "#EXT-X-START", "#EXT-X-DEFINE", "#EXTINF", "#EXT-X-BYTERANGE", "#EXT-X-VERSION", "#EXT-X-ENDLIST", "#EXTM3U", "#EXT-X-FOO", "#ext-x-key", "#EXT", "#EXT-X-DATERANGE", "#"];
  const separators = [":", " :", ": ", " ", "\t", "", ":\t"];
  const values = [
    'X="//secret.example/p"',
    "//secret.example/p",
    '"https://secret.example/p"',
    'URI="https://secret.example/p"',
    "URI=https://secret.example/p",
    'URI="//secret.example/p",X=1',
    "X=secret.example/p",
    "6,secret.example",
    "6 secret.example",
    '{$tok}',
    'NAME="{$tok}",VALUE="secret.example"',
    'NAME="tok",VALUE="secret.example",X="{$tok}"',
    "100@secret",
    'uri="https://secret.example/p"',
    "",
  ];
  it("ни одна комбинация не выпускает секрет", () => {
    let combinations = 0;
    for (const name of names) {
      for (const separator of separators) {
        for (const value of values) {
          combinations += 1;
          const line = `${name}${separator}${value}`;
          assertNoLeak(`#EXTM3U\n${line}\n#EXTINF:1,\nok.ts\n`, line);
          assertNoLeak(`#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO\n${line}\n`, `мастер: ${line}`);
        }
      }
    }
    assert.equal(combinations, names.length * separators.length * values.length);
  });
});
