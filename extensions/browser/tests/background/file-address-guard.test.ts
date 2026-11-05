// Проверка адреса файла (решение 199): чистые функции. Имена хостов берём из настоящего разбора
// `URL`, чтобы хитрые записи адреса проверялись в той форме, в какой они придут в фон.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { decideFileAddress, isLocalHost, localNetworkAccessOf, NO_LOCAL_ADDRESS_CONSENT, ruleAllowsFileUrl } from "../../src/background/handoff/file-address-guard.ts";
import { LOCAL_ADDRESS_HOST_SAMPLES, PUBLIC_ADDRESS_HOST_SAMPLES } from "./local-address-samples.ts";

const hostOf = (url: string): string => new URL(url).hostname;

describe("isLocalHost", () => {
  for (const url of LOCAL_ADDRESS_HOST_SAMPLES) it(`локальный: ${url}`, () => assert.equal(isLocalHost(hostOf(url)), true, hostOf(url)));
  for (const url of PUBLIC_ADDRESS_HOST_SAMPLES) it(`публичный: ${url}`, () => assert.equal(isLocalHost(hostOf(url)), false, hostOf(url)));

  it("пустой хост (`about:blank`, `data:`) не локальный: такая страница не получает прав локальной", () => {
    assert.equal(isLocalHost(""), false);
  });

  it("неразобранный адрес в скобках считается локальным: в сомнении запрещаем", () => {
    assert.equal(isLocalHost("[not-an-address]"), true);
    assert.equal(isLocalHost("[1:2:3]"), true);
    assert.equal(isLocalHost("[1:2:3:4:5:6:7:8:9]"), true);
  });
});

describe("decideFileAddress", () => {
  const PUBLIC_PAGE = "video.example.com";
  const LOCAL_PAGE = "jellyfin.local";
  const LOCAL_FILE = "192.168.1.5";
  const PUBLIC_FILE = "cdn.example.net";

  it("публичный файл разрешён всем при любой настройке", () => {
    for (const setting of ["block", "ask"] as const) assert.equal(decideFileAddress(PUBLIC_PAGE, PUBLIC_FILE, setting, NO_LOCAL_ADDRESS_CONSENT).kind, "allowed");
  });

  it("локальная страница ходит на локальный файл при любой настройке", () => {
    for (const setting of ["block", "ask"] as const) assert.equal(decideFileAddress(LOCAL_PAGE, LOCAL_FILE, setting, NO_LOCAL_ADDRESS_CONSENT).kind, "allowed");
  });

  it("публичная страница, настройка «блокировать»: отказ, и согласие его не отменяет", () => {
    assert.equal(decideFileAddress(PUBLIC_PAGE, LOCAL_FILE, "block", NO_LOCAL_ADDRESS_CONSENT).kind, "refused");
    assert.equal(decideFileAddress(PUBLIC_PAGE, LOCAL_FILE, "block", { kind: "given_for", host: LOCAL_FILE }).kind, "refused");
  });

  it("публичная страница, настройка «спрашивать»: без согласия вопрос, с согласием можно", () => {
    assert.equal(decideFileAddress(PUBLIC_PAGE, LOCAL_FILE, "ask", NO_LOCAL_ADDRESS_CONSENT).kind, "needs_consent");
    assert.equal(decideFileAddress(PUBLIC_PAGE, LOCAL_FILE, "ask", { kind: "given_for", host: LOCAL_FILE }).kind, "allowed");
  });

  it("согласие на другой хост — не согласие: человек получит новый вопрос", () => {
    assert.equal(decideFileAddress(PUBLIC_PAGE, "192.168.1.1", "ask", { kind: "given_for", host: LOCAL_FILE }).kind, "needs_consent");
  });

  it("страница без хоста считается публичной", () => {
    assert.equal(decideFileAddress("", LOCAL_FILE, "block", NO_LOCAL_ADDRESS_CONSENT).kind, "refused");
  });
});

describe("ruleAllowsFileUrl", () => {
  const rule = { pageHost: "video.example.com", setting: "ask", consentedHost: "192.168.1.5" } as const;

  it("согласие действует только на тот хост, про который человек сказал «да»", () => {
    assert.equal(ruleAllowsFileUrl(rule, "http://192.168.1.5/movie.mp4"), true);
    assert.equal(ruleAllowsFileUrl(rule, "http://192.168.1.1/admin"), false);
  });

  it("публичный адрес проходит, неразбираемый — нет", () => {
    assert.equal(ruleAllowsFileUrl(rule, "https://cdn.example.net/a.mp4"), true);
    assert.equal(ruleAllowsFileUrl(rule, "not a url"), false);
  });
});

describe("localNetworkAccessOf", () => {
  it("локальная страница получает всё, что разрешает решение 199", () => {
    assert.deepEqual(localNetworkAccessOf({ pageHost: "nas.local", setting: "block", consentedHost: null }), { kind: "any" });
  });

  it("публичная страница получает только хост согласия, а без него ничего", () => {
    assert.deepEqual(localNetworkAccessOf({ pageHost: "video.example.com", setting: "ask", consentedHost: "192.168.1.5" }), { kind: "host", host: "192.168.1.5" });
    assert.deepEqual(localNetworkAccessOf({ pageHost: "video.example.com", setting: "ask", consentedHost: null }), { kind: "none" });
  });

  it("публичный хост в согласии заслон не открывает", () => {
    assert.deepEqual(localNetworkAccessOf({ pageHost: "video.example.com", setting: "ask", consentedHost: "cdn.example.net" }), { kind: "none" });
  });
});
