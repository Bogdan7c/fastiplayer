# S32A: аудит AES-128 зависимостей

Дата проверки: 2026-07-23.

## Решение

HLS AES-128 реализован за маленькой first-party границей `web-media-hls`.
Внешние crates не видны parser-у, transport-у, demux-у, player-у или UI:

- `aes = 0.9.1`, `default-features = false`, feature `zeroize`;
- `cbc = 0.2.1`, `default-features = false`, features `block-padding`, `zeroize`;
- `zeroize = 1.9.0`, `default-features = false`, feature `alloc`.

Версии прибиты exact constraints в workspace manifest. CBC используется только
потому, что RFC 8216 требует AES-128-CBC с PKCS#7; это не новый общий crypto API.
RustCrypto справедливо помечает CBC как unauthenticated hazmat. HLS initial
profile не предоставляет authentication tag, поэтому boundary fail-closed
проверяет длину ciphertext и строгий PKCS#7, но не заявляет аутентификацию.

## Upstream и поддержка

Все выбранные crates принадлежат активным RustCrypto repositories:

- `aes`: `RustCrypto/block-ciphers`;
- `cbc`: `RustCrypto/block-modes`;
- `cipher`, `crypto-common`: `RustCrypto/traits`;
- `block-buffer`, `block-padding`, `cpubits`, `inout`, `zeroize`:
  `RustCrypto/utils`;
- `hybrid-array`: `RustCrypto/hybrid-array`.

На дату аудита `aes 0.9.1`, `cbc 0.2.1` и `zeroize 1.9.0` являются актуальными
релизами выбранных линий; `cbc 0.2.1` опубликован 2026-07-19. Git dependencies,
forks и local patches не добавлены.

## License и MSRV

Все новые узлы используют `MIT OR Apache-2.0`, что допускается `deny.toml`.
Заявленный upstream MSRV для direct и новых transitive crates — Rust 1.85:

- `aes 0.9.1`;
- `cbc 0.2.1`;
- `cipher 0.5.2`;
- `block-buffer 0.12.1`;
- `block-padding 0.4.2`;
- `cpubits 0.1.1`;
- `crypto-common 0.2.2`;
- `hybrid-array 0.4.13`;
- `inout 0.2.2`;
- `zeroize 1.9.0`.

Это ниже project MSRV 1.92. Реальная workspace-сборка отдельно проверяется на
Rust 1.92 и primary Rust 1.96.

## Feature, native и transitive delta

`aes` не включает `hazmat`; `cbc` получает только обязательный padding API.
`alloc` включён у `zeroize`, потому что service request material очищает
эфемерные `String`, а decrypted media buffer — `Vec<u8>`. `std`, serde, rand,
getrandom и derive features не включены напрямую.

Lock delta добавляет pure-Rust crates:

- `aes 0.9.1`;
- `cbc 0.2.1`;
- `cipher 0.5.2`;
- `block-buffer 0.12.1`;
- `block-padding 0.4.2`;
- `cpubits 0.1.1`;
- `crypto-common 0.2.2`;
- `hybrid-array 0.4.13`;
- `inout 0.2.2`.

`zeroize` обновлён с 1.8.2 до 1.9.0 в рамках совместимого `1.x`; его MSRV стал
1.85 и остаётся ниже project MSRV. `cpufeatures 0.3.0` уже присутствовал через
`rand/chacha20`; AES переиспользует его. Native libraries, C/C++, bindgen,
pkg-config, runtime dynamic loading, filesystem/network access и build scripts
новый crypto path не добавляет.

## Security boundary

- Key file принимается только при exact длине 16 bytes.
- Key и plaintext хранятся в `Zeroizing`; extractor request secrets также
  очищаются на drop.
- CBC перезапускается на каждом segment boundary с explicit IV либо
  zero-left-padded big-endian media sequence.
- Encrypted `EXT-X-MAP` без explicit IV rejected до fetch/decrypt.
- Invalid ciphertext length и invalid PKCS#7 различаются typed, но ни одна
  ошибка не содержит key, IV, URI или ciphertext.
- `SAMPLE-AES`, иной METHOD и non-identity KEYFORMAT rejected profile/key-state
  validation до runtime/player mutation.

## Verification

Обязательные команды и их фактический результат фиксируются в S32A handoff.
Минимальный blocking набор:

- affected tests на Rust 1.96;
- affected strict Clippy и rustdoc;
- workspace check на Rust 1.92 и 1.96;
- `cargo deny check advisories licenses bans sources`;
- `cargo machete --with-metadata`;
- format/refactor guardrails;
- coverage inventory/ratchet;
- `git diff --check`.
