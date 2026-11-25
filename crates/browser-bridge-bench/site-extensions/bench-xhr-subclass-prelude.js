// Prelude стенда: «чужой код раньше нашего скрипта». Ставится в MAIN-мир ПЕРЕД `page.js` экстеншена
// (см. `src/scenarios/extension_prelude.rs`) и повторяет блокировщик рекламы на YouTube: подменяет
// `XMLHttpRequest` подклассом, у которого на прототипе нет своих `status` / `responseURL` /
// `responseType`, а `open`/`send`/`response*`/`getResponseHeader` просто зовут `super`.
(() => {
  const NativeXMLHttpRequest = self.XMLHttpRequest;
  // Родной ли `open` в момент подмены. Если наш скрипт успел раньше и обернул `open`, строка
  // «чужой код раньше нашего» была бы ложной: страница проверит этот факт сама.
  const nativeOpenAtInstall = /\[native code\]/.test(
    Function.prototype.toString.call(NativeXMLHttpRequest.prototype.open),
  );
  self.XMLHttpRequest = class extends NativeXMLHttpRequest {
    open(method, url, ...args) {
      return super.open(method, url, ...args);
    }
    send(...args) {
      return super.send(...args);
    }
    get response() {
      return super.response;
    }
    get responseText() {
      return super.responseText;
    }
    getResponseHeader(name) {
      return super.getResponseHeader(name);
    }
    getAllResponseHeaders() {
      return super.getAllResponseHeaders();
    }
  };
  // Факт предусловия для страницы (`subtitles-xhr-subclass`).
  window.__benchXhrSubclass = { installed: true, nativeOpenAtInstall };
})();
