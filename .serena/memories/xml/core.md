# Hardened XML Boundary

## S36D Smooth Streaming VOD consumer (2026-07-24)

- `smooth-streaming-manifest-core` is the sealed MS-SSTR VOD schema/profile owner and consumes only `bounded-xml-reader` project events with caller-supplied XML and schema budgets. It adds no second XML parser or I/O.
- Smooth schema names must be unqualified; namespaced items are typed private extensions, unknown unqualified items are unsupported constructs, and Protection is typed DRM. DTD/external entities remain exact S04X failures with the original `XmlReadError` source.
- Full profile, timeline, codec and cancellation contract: `mem:media-services/smooth-streaming-vod-s36d-2026-07-24`.

## S34 DASH consumer (2026-07-24)

- `dash-mpd-core` is the static DASH schema/profile owner and consumes only `bounded-xml-reader` project events with caller-supplied XML and schema budgets. It adds no second XML parser or hidden I/O; dynamic MPD, DTD/entities, ContentProtection and unsupported required constructs fail closed. Full profile: `mem:media-services/dash-vod-s34-2026-07-24`.

## S04X (2026-07-20)

- `bounded-xml-reader` — единственная project-owned граница для будущего parsing untrusted XML. Она принимает только caller-owned `&[u8]`, поэтому не владеет filesystem/network I/O и не может скрыто загружать external entities/resources.
- Вызывающий код обязан собрать `XmlBudgets` через named builder и явно задать все лимиты: document bytes, nesting depth, event tokens, attributes per element/total/bytes, namespace declarations per element/total/bytes и decoded text bytes. Скрытых production defaults нет.
- Boundary использует `quick-xml 0.41`, но не экспортирует его типы. Наружу выходят только project-owned `XmlEvent`, element/attribute/text и resolved expanded-name vocabulary. XSPF/DASH/ISM/HDS владельцы позже отвечают за schema, allowed namespace URIs и format semantics.
- XML policy: UTF-8 XML 1.0 only; DOCTYPE/DTD, external/custom entities и undeclared prefixes rejected; пять predefined XML entities и valid numeric character references legal. XML 1.1 rejected до отдельного полного контракта.
- Reader fail-closed и fused после terminal error; document grammar допускает ровно один root, проверяет declaration placement/order/duplicates, exact XML whitespace outside root и namespace constraints.
- Focused fixtures/tests находятся в `crates/bounded-xml-reader/tests` и покрывают entity/doctype/depth/attribute/namespace bombs, caller budgets, declarations, namespace resolution и terminal errors.
- Уязвимый transitively pinned `quick-xml 0.39.3` закрыт локальным exact-source replacement `crates/wayland-scanner-patch` версии 0.31.10, обновлённым только до `quick-xml 0.41` и совместимого `xml_content(XmlVersion::Implicit1_0)`. Removal gate и provenance — `mem:dependency-patches/core` и `docs/dependency-patches.toml`.
