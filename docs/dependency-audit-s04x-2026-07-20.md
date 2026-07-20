# S04X: аудит safe XML parser и advisory closure

Дата проверки: 2026-07-20.

## Итог

RUSTSEC-2026-0194 и RUSTSEC-2026-0195 закрыты без ignore и без обновления
`winit`/`egui`/Wayland stack. В workspace graph остался один `quick-xml 0.41.0`,
который одновременно используется:

- project-owned `bounded-xml-reader`;
- local `wayland-scanner 0.31.10` patch;
- существующими Wayland dependents через неизменённые public versions.

## Official source и security evidence

- Crate: `quick-xml 0.41.0`.
- Registry: <https://crates.io/crates/quick-xml/0.41.0>.
- Source/tag: <https://github.com/tafia/quick-xml/tree/v0.41.0>.
- Tag commit: `4deda08abeffdc188c269360229cf47e12a77a9f`.
- License: MIT.
- Declared MSRV: Rust 1.79; project MSRV Rust 1.92 совместим.
- Default features: пустые.
- Единственная normal dependency при выключенных optional features: `memchr`;
  она уже присутствовала в workspace graph.
- `RUSTSEC-2026-0194`: duplicate attribute checking до 0.41 мог работать за
  `O(N²)`; 0.41 делает проверку линейной.
- `RUSTSEC-2026-0195`: `NsReader` до 0.41 не ограничивал число namespace
  declarations до выдачи event-а; 0.41 добавляет
  `NamespaceResolver::set_max_declarations_per_element`.

Context7 и official `quick-xml` docs подтвердили event API:
`Start`/`Empty`/`End`/`Text`/`CData`/`DocType`/`GeneralRef`, separate namespace
resolver и explicit predefined/numeric entity handling.

## Почему потребовался local wayland-scanner patch

Последний опубликованный `wayland-scanner 0.31.10`:

- MIT;
- MSRV Rust 1.71;
- требует несовместимый range `quick-xml = "0.39"`;
- является единственным источником vulnerable `quick-xml` в старом graph.

Upstream commit
<https://github.com/Smithay/wayland-rs/commit/d07c4f91f28b42e5a485823ffd9d8d5a210b1053>
уже переводит current development branch на `quick-xml 0.41`, но crates.io
release с этим изменением отсутствовал во время S04X.

Local patch создан из exact crates.io archive checksum
`9c324a910fd86ebdc364a3e61ec1f11737d3b1d6c273c0239ee8ff4bc0d24b4a`.
Owned diff ограничен:

- `Cargo.toml`;
- `Cargo.toml.orig`;
- одним `src/parse.rs` callsite, где `xml_content` получает
  `XmlVersion::Implicit1_0`.

Patch исключён из workspace, имеет собственный lock и direct locked tests.
Удаление обязательно после опубликованного upstream release, допускающего
`quick-xml >=0.41`, и успешных workspace/Wayland checks.

## Project-owned boundary

Crate `bounded-xml-reader` принимает только `&[u8]`. Это compile-time boundary:
он не принимает path, URL, `Read`, callback загрузчика или entity resolver,
поэтому не может выполнить скрытый filesystem/network I/O.

Caller обязан явно задать:

- document bytes;
- depth;
- tokens;
- attributes per element;
- total attribute count и materialized attribute bytes;
- namespace declarations per element;
- total namespace declaration count и prefix/URI bytes;
- decoded text bytes.

Boundary:

- ограничивает bytes до parser startup;
- передаёт per-element namespace limit внутрь `quick-xml 0.41` resolver-а;
- публикует только project-owned namespace-resolved events;
- линейно отвергает duplicate attributes по expanded name даже при разных
  namespace prefixes;
- отклоняет DTD/DOCTYPE, external/custom entities и unknown prefixes;
- разрешает numeric references и пять predefined XML entities;
- проверяет single-root document grammar и XML declaration;
- принимает только UTF-8 XML 1.0;
- учитывает comments и processing instructions в token budget, но не публикует
  их domain parser-ам;
- не знает XSPF, DASH, ISM, HDS или допустимые domain namespace URI.

## Verification contract

Focused:

```bash
cargo test -p bounded-xml-reader --locked
cargo clippy -p bounded-xml-reader --all-targets -- -D warnings
cargo test --manifest-path crates/wayland-scanner-patch/Cargo.toml --locked
python3 scripts/check-dependency-patches.py
```

Dependency/advisory:

```bash
cargo tree -i quick-xml --workspace --all-features
cargo deny check
```

Workspace/MSRV:

```bash
cargo check --workspace --all-features --locked
cargo +1.92.0 check -p bounded-xml-reader --locked
cargo fmt --all --check
```

Malicious checked-in fixtures покрывают external/internal DOCTYPE, custom
entity, nesting depth, attribute bomb и namespace-declaration bomb.
