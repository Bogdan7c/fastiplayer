// Тесты манифестов и сборки: идентичность из фикстуры, набор прав, целостность dist, детерминизм.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";
import { buildTarget } from "../scripts/build.ts";
import { loadIdentity, mergeManifestParts, type TargetBrowser } from "../scripts/manifest.ts";
import { NATIVE_HOST_NAME } from "../src/shared/host-name.ts";

type Manifest = Record<string, any>;

const identity = loadIdentity();
const outputRoot = mkdtempSync(join(tmpdir(), "fastiplayer-bridge-build-"));
const targets: readonly TargetBrowser[] = ["chrome", "firefox"];

async function builtManifest(target: TargetBrowser): Promise<Manifest> {
  const dir = await buildTarget(target, outputRoot);
  return JSON.parse(readFileSync(join(dir, "manifest.json"), "utf8")) as Manifest;
}

// ID Chrome из публичного ключа: SHA-256, первые 16 байт, каждая hex-цифра 0..f -> a..p.
function chromeExtensionIdFromKey(base64Key: string): string {
  const digest = createHash("sha256").update(Buffer.from(base64Key, "base64")).digest("hex").slice(0, 32);
  return [...digest].map((digit) => String.fromCharCode("a".charCodeAt(0) + parseInt(digit, 16))).join("");
}

function listFiles(dir: string): Map<string, Buffer> {
  const files = new Map<string, Buffer>();
  for (const entry of readdirSync(dir, { withFileTypes: true, recursive: true })) {
    if (entry.isFile()) {
      const path = join(entry.parentPath, entry.name);
      files.set(path.slice(dir.length), readFileSync(path));
    }
  }
  return files;
}

describe("идентичность экстеншена", () => {
  it("имя хоста в коде совпадает с фикстурой протокола", () => {
    assert.equal(NATIVE_HOST_NAME, identity.nativeHostName);
  });

  it("ID Chrome, выведенный из ключа, равен ожидаемому", () => {
    assert.equal(chromeExtensionIdFromKey(identity.chromiumManifestKey), "mcikhhmaenodlfkafgehbkikilmpnpji");
    assert.equal(identity.chromiumExtensionId, "mcikhhmaenodlfkafgehbkikilmpnpji");
  });

  it("манифест Chrome несёт ключ из фикстуры, Firefox — gecko id из фикстуры", async () => {
    const chrome = await builtManifest("chrome");
    const firefox = await builtManifest("firefox");
    assert.equal(chrome["key"], identity.chromiumManifestKey);
    assert.equal(firefox["key"], undefined);
    assert.equal(firefox["browser_specific_settings"].gecko.id, identity.geckoId);
    assert.equal(chrome["browser_specific_settings"], undefined);
  });
});

describe("манифесты", () => {
  it("права ровно ожидаемые, без cookies и webRequest", async () => {
    for (const target of targets) {
      const manifest = await builtManifest(target);
      assert.deepEqual([...manifest["permissions"]].sort(), ["contextMenus", "nativeMessaging", "scripting", "storage"]);
      assert.deepEqual(manifest["host_permissions"], ["<all_urls>"]);
      assert.equal(manifest["manifest_version"], 3);
      assert.equal(manifest["name"], "Fastiplayer Bridge");
    }
  });

  it("различия браузеров: service_worker и минимум Chrome; scripts, CSP и data_collection у Firefox", async () => {
    const chrome = await builtManifest("chrome");
    const firefox = await builtManifest("firefox");
    assert.equal(chrome["background"].service_worker, "background.js");
    assert.equal(chrome["background"].scripts, undefined);
    assert.equal(chrome["minimum_chrome_version"], "148");
    // Байты видео идут контент-скрипт → фон структурным клонированием (Chrome 148+), не JSON.
    assert.equal(chrome["message_serialization"], "structured_clone");
    assert.equal(firefox["message_serialization"], undefined);
    assert.deepEqual(firefox["background"].scripts, ["background.js"]);
    assert.equal(firefox["background"].service_worker, undefined);
    assert.match(firefox["content_security_policy"].extension_pages, /connect-src [^;]*ws:\/\/127\.0\.0\.1:\*/);
    assert.deepEqual(firefox["browser_specific_settings"].gecko.data_collection_permissions, { required: ["none"] });
  });

  it("контент-скрипты: изолированный и MAIN, оба в document_start для всех фреймов", async () => {
    const manifest = await builtManifest("chrome");
    const scripts: Manifest[] = manifest["content_scripts"];
    assert.equal(scripts.length, 2);
    assert.equal(scripts.find((script) => script["js"][0] === "content.js")?.["world"], undefined);
    assert.equal(scripts.find((script) => script["js"][0] === "page.js")?.["world"], "MAIN");
    for (const script of scripts) {
      assert.equal(script["run_at"], "document_start");
      assert.equal(script["all_frames"], true);
      assert.equal(script["match_about_blank"], true);
      assert.equal(script["match_origin_as_fallback"], true);
    }
  });

  it("слияние не позволяет различиям молча переопределить базу", () => {
    assert.throws(() => mergeManifestParts({ name: "a" }, { name: "b" }), /manifest conflict at name/);
    assert.deepEqual(mergeManifestParts({ a: { x: 1 } }, { a: { y: 2 } }), { a: { x: 1, y: 2 } });
  });
});

describe("сборка dist", () => {
  it("каждый файл, на который ссылается манифест, существует, и лишних файлов нет", async () => {
    for (const target of targets) {
      const manifest = await builtManifest(target);
      const dir = join(outputRoot, target);
      const referenced = new Set<string>(["manifest.json"]);
      const background = manifest["background"];
      for (const file of [background.service_worker, ...(background.scripts ?? [])]) if (file) referenced.add(file);
      for (const script of manifest["content_scripts"]) for (const file of script["js"]) referenced.add(file);
      referenced.add(manifest["action"].default_popup);
      referenced.add(manifest["options_ui"].page);
      for (const file of Object.values<string>(manifest["icons"])) referenced.add(file);
      for (const file of Object.values<string>(manifest["action"].default_icon)) referenced.add(file);
      for (const file of referenced) assert.ok(existsSync(join(dir, file)), `${target}: нет файла ${file}`);
      // Скрипты, подключённые из html, тоже обязаны быть в dist.
      for (const page of ["popup", "options"]) {
        const html = readFileSync(join(dir, `${page}.html`), "utf8");
        const scriptSrc = /<script src="([^"]+)"/.exec(html)?.[1];
        assert.ok(scriptSrc !== undefined && existsSync(join(dir, scriptSrc)), `${target}: ${page}.html без скрипта`);
        referenced.add(scriptSrc);
      }
      const actual = [...listFiles(dir).keys()].map((path) => path.replace(/^\//, ""));
      assert.deepEqual(actual.filter((path) => !referenced.has(path)), [], `${target}: лишние файлы в dist`);
    }
  });

  it("повторная сборка даёт те же байты", async () => {
    for (const target of targets) {
      await buildTarget(target, outputRoot);
      const first = listFiles(join(outputRoot, target));
      await buildTarget(target, outputRoot);
      const second = listFiles(join(outputRoot, target));
      assert.deepEqual([...second.keys()].sort(), [...first.keys()].sort());
      for (const [path, bytes] of first) assert.ok(bytes.equals(second.get(path)!), `${target}: ${path} изменился`);
    }
  });

  it("фон и изолированный скрипт собраны в IIFE и знают порт передачи и функцию выбора видео", async () => {
    for (const target of targets) {
      for (const part of ["background.js", "content.js"]) {
        const bundle = readFileSync(join(outputRoot, target, part), "utf8");
        assert.match(bundle, /fastiplayer-handoff/, `${target}/${part}: нет имени порта передачи`);
        assert.match(bundle, /fastiplayerBridgeDescribeVideoCandidates/, `${target}/${part}: нет функции выбора видео`);
        assert.doesNotMatch(bundle, /^\s*(import|export)\s/m);
      }
    }
  });

  it("попап собран в IIFE и содержит имя порта статуса", async () => {
    const popup = readFileSync(join(outputRoot, "chrome", "popup.js"), "utf8");
    assert.match(popup, /bridge-status/);
    assert.doesNotMatch(popup, /^\s*(import|export)\s/m);
  });
});
