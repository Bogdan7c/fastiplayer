# AUD-016 — DMA-BUF frame-contract validation до unsafe import (2026-08-23)

## Независимое подтверждение

- Detached worktree verification с production `DmaBufWgpuFrameMaterializer`, fake provider, recording importer и настоящим `VideoFrameLease` доказала pre-fix defect.
- Topology-valid `ComposedLayers 800x450` при decoded frame `ComposedLayers 640x360` и topology-valid `SeparateLayers 640x360` при contract `ComposedLayers` оба доходили до importer-а: `importer_calls=1`.
- Валидный `ComposedLayers 640x360` также доходил до importer-а. Во всех сценариях lease release был exactly once.
- Root cause: materializer вызывал только `validate_dma_buf_descriptor_import_topology`; importer не получает `DecodedFrame` и не может сравнить descriptor с frame contract/coded dimensions.
- Context7 `/gfx-rs/wgpu/wgpu-v29.0.1` подтвердил caller-owned unsafe contract: descriptor для `create_texture_from_hal` обязан соответствовать существующему HAL resource; wgpu-hal выполняет minimal validation.

## Ownership и boundaries после исправления

- `video-core` владеет renderer-neutral typed boundary `validate_dma_buf_descriptor_against_frame_contract(contract, coded_width, coded_height, descriptor)`.
- Boundary проверяет: валидный `VideoFrameContract`, положительные coded dimensions, DMA-BUF hardware-zero-copy transfer, object/layer topology, точный `DmaBufImageLayout` и точные descriptor dimensions.
- `DmaBufDescriptorRejection` теперь также различает `InvalidFrameContract`, `InvalidCodedSize`, `FrameContractRequiresHostUpload`, `ImageLayoutMismatch` и `CodedSizeMismatch`. Старые topology variants остаются без изменения.
- Старый общий `validate_resource_descriptor_against_contract` сохраняет `anyhow::Result` и прежний topology context; host path/API semantics не менялись.
- `DmaBufWgpuFrameMaterializer::try_texture_view_lookup` обязан вызывать полную typed boundary сразу после provider descriptor lookup и до texture-cache lock/`DmaBufImporter`.
- Любой mismatch возвращается через существующий `WgpuFrameTextureViewLookup::Unsupported { reason: DmaBufDescriptorRejected(..) }`; app получает typed rejection.
- Materializer никогда не освобождает provider resource сам. `VideoFrameLease` остаётся единственным release owner и освобождает resource exactly once при drop последнего lease. Не добавлять manual release на contract rejection.
- Internal `DmaBufTextureImporter` seam в render crate существует для functional boundary tests; production implementation делегирует тому же `DmaBufImporter`.

## Focused functional tests

В `crates/render-wgpu-video/src/video/dma_buf_materializer.rs`:
- `wrong_coded_dimensions_are_rejected_before_dma_buf_import`;
- `incompatible_layout_is_rejected_before_dma_buf_import`;
- `valid_descriptor_reaches_dma_buf_import_without_release_regression`.

Первые два требуют exact typed rejection, `import_calls == 0`, отсутствие раннего release и `release_calls == 1` после drop lease. Валидный control требует `import_calls == 1` и то же exactly-once accounting.

В `crates/video-core/src/resource/tests.rs` focused tests закрепляют valid pair, typed layout/size mismatch, wrong transfer path/zero size и propagation topology rejection.

## Проверка

- `cargo +1.96.0 test -p video-core -p render-wgpu-video --locked`: video-core 55/55, render-wgpu-video 103/103.
- `cargo test -p app-egui --locked`: 962/962.
- `cargo +1.96.0 check --workspace --locked`: PASS.
- `cargo +1.96.0 clippy -p app-egui --all-targets --locked -- -D warnings`: PASS.
- `cargo fmt --all --check`: PASS.

## Ограничение

Реальный Vulkan/HAL import и конкретный VA-API exporter/driver frequency в автоматическом regression не измеряются. Recording fake расположен непосредственно на production importer boundary и доказывает control-flow/release contract без GPU dependency.