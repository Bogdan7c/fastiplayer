// TEST-ONLY. base64 для моста контент-скрипт -> фон. Chromium гонит сообщения портов через JSON
// (ArrayBuffer не проходит), Firefox умеет structured clone; один путь base64 для обоих проще и
// одинаково проверяется. Кусками по 32 КиБ, чтобы fromCharCode.apply не упёрся в лимит аргументов.
(function (root) {
  "use strict";
  const CHUNK = 0x8000;

  function bytesToBase64(bytes) {
    let binary = "";
    for (let offset = 0; offset < bytes.length; offset += CHUNK) {
      binary += String.fromCharCode.apply(null, bytes.subarray(offset, offset + CHUNK));
    }
    return btoa(binary);
  }

  function base64ToBytes(text) {
    const binary = atob(text);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
    return bytes;
  }

  const api = { bytesToBase64, base64ToBytes };
  root.__fpBench = Object.assign(root.__fpBench || {}, api);
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(globalThis);
