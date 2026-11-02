#!/usr/bin/env python3
"""Собирает manifest.json расширения из шаблона и fixtures/extension-identity.json.

Ключ Chromium и gecko id живут ТОЛЬКО в фикстуре протокола (единый источник правды);
шаблон содержит подстановки @MANIFEST_KEY@ и @GECKO_ID@. Если подстановка не удалась
(поле отсутствует или в шаблоне остался «@...@») — падаем с ошибкой, а не молчим.
Использование: make-manifest.py <identity.json> <template> <выходной manifest.json>
"""
import json
import sys


def main() -> int:
    identity_path, template_path, output_path = sys.argv[1:4]
    with open(identity_path, encoding="utf-8") as handle:
        identity = json.load(handle)
    substitutions = {
        "@MANIFEST_KEY@": identity["chromium_development"]["manifest_key"],
        "@GECKO_ID@": identity["firefox"]["gecko_id"],
    }
    with open(template_path, encoding="utf-8") as handle:
        text = handle.read()
    for placeholder, value in substitutions.items():
        if placeholder not in text:
            sys.exit(f"make-manifest: placeholder {placeholder} missing in template")
        text = text.replace(placeholder, value)
    if "@" in text.replace(substitutions["@GECKO_ID@"], ""):
        sys.exit("make-manifest: unresolved '@' placeholder left in manifest")
    json.loads(text)  # проверка, что получился валидный JSON
    with open(output_path, "w", encoding="utf-8") as handle:
        handle.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
