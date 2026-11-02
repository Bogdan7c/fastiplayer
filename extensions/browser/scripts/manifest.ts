// Сборка манифеста: общая база + различия браузера + идентичность из фикстуры протокола.
// Чистые функции, без файловой системы (кроме loadIdentity) — их проверяют тесты.
import { readFileSync } from "node:fs";
import { join } from "node:path";

export type TargetBrowser = "chrome" | "firefox";
export type JsonObject = { [key: string]: unknown };

// Идентичность экстеншена: единственный источник — фикстура протокола (решение 154).
export type ExtensionIdentity = {
  readonly nativeHostName: string;
  readonly chromiumManifestKey: string;
  readonly chromiumExtensionId: string;
  readonly geckoId: string;
};

export const extensionRoot = join(import.meta.dirname, "..");
export const identityFixturePath = join(extensionRoot, "../../crates/browser-bridge-protocol/fixtures/extension-identity.json");

export function loadIdentity(): ExtensionIdentity {
  const raw = JSON.parse(readFileSync(identityFixturePath, "utf8")) as {
    native_host_name: string;
    chromium_development: { manifest_key: string; extension_id: string };
    firefox: { gecko_id: string };
  };
  return {
    nativeHostName: raw.native_host_name,
    chromiumManifestKey: raw.chromium_development.manifest_key,
    chromiumExtensionId: raw.chromium_development.extension_id,
    geckoId: raw.firefox.gecko_id,
  };
}

function isPlainObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

// Глубокое слияние: объекты сливаются, массивы и скаляры заменяются целиком.
// Ключ, который есть и в базе и в различиях с разными скалярами, — ошибка конфигурации сборки
// (различия должны ДОБАВЛЯТЬ, а не молча переопределять базу), поэтому бросаем исключение.
export function mergeManifestParts(base: JsonObject, overlay: JsonObject, path = ""): JsonObject {
  const merged: JsonObject = { ...base };
  for (const [key, overlayValue] of Object.entries(overlay)) {
    const baseValue = merged[key];
    if (baseValue === undefined) {
      merged[key] = overlayValue;
    } else if (isPlainObject(baseValue) && isPlainObject(overlayValue)) {
      merged[key] = mergeManifestParts(baseValue, overlayValue, `${path}${key}.`);
    } else {
      throw new Error(`manifest conflict at ${path}${key}: overlay must not override base`);
    }
  }
  return merged;
}

export function buildManifest(
  target: TargetBrowser,
  baseManifest: JsonObject,
  browserManifest: JsonObject,
  identity: ExtensionIdentity,
  version: string,
): JsonObject {
  const merged = mergeManifestParts(baseManifest, browserManifest);
  const withVersion: JsonObject = { ...merged, version };
  if (target === "chrome") {
    // Ключ даёт стабильный ID экстеншена при «Load unpacked» и в упакованном виде.
    return { ...withVersion, key: identity.chromiumManifestKey };
  }
  const settings = withVersion["browser_specific_settings"] as JsonObject;
  const gecko = settings["gecko"] as JsonObject;
  return { ...withVersion, browser_specific_settings: { ...settings, gecko: { id: identity.geckoId, ...gecko } } };
}
