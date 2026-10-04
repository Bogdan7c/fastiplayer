# AV1 global-motion arithmetic fix (2026-09-09)

## Follow-up 2026-10-04: настоящий корень — разбор global-motion параметров

Тот же ролик (формат 399) вешал AMD VCN (`ring vcn_unified_0 timeout` → Mesa abort «context is lost») при обычном воспроизведении/seek; точка зависания «плавала». Корень: `Reader::decode_subexp` (`cros-codecs-patch/src/codec/av1/reader.rs`) в ветке `subexp_final_bits` возвращал значение без `+ mk` (AV1 spec 5.9.28) → большие global-motion translation приходили в VA с ошибкой ровно 2048 << precDiff, драйвер получал неверную warp-матрицу. Второй spec-дефект там же: `setup_past_independence` в `parse_global_motion_params` сбрасывал PrevGmParams полуоткрытыми диапазонами (без ALTREF и без параметра 5); теперь `Last..=AltRef`, `0..=5`. Вероятно, переполнение shear 09-09 тоже было следствием этих мусорных значений; wide-арифметика остаётся корректной защитой.

Метод доказательства: `LIBVA_TRACE` ffmpeg vs headless VA-API AV1 adapter на одном IVF — до фикса первое расхождение на кадре 36 (`wm[3].wmmat[0]`), после фикса все 5433 VAPictureParameterBufferAV1 совпадают с ffmpeg, 0 VCN timeout; приложение с seek-ами по YouTube и локальному фрагменту — без зависаний. Регрессии: `parser::tests::global_motion_translation_keeps_subexp_offset_for_large_values`, `global_motion_without_primary_ref_uses_identity_for_altref` (оба падают с предсказанными значениями при откате фикса). Диагностический приём на будущее: зависание VCN на одном потоке при работающих ffmpeg/mpv → сравнивать `LIBVA_TRACE` параметров, а не гадать про surfaces/seek.

## Root cause and implemented repair

The owner-supplied YouTube video JrT1PjOjOjc selects yt-dlp format 399: AV1 Main 1920x1080/50, video-only MP4, original size 382121214 bytes. It reproducibly panicked in the local cros-codecs AV1 parser during VA-API playback: `Parser::setup_shear` multiplied global-motion values in i32 before AV1 Round2Signed, then a collapsed decoder thread surfaced as `Decoder thread disconnected`.

The repair is intentionally local to `crates/cros-codecs-patch/src/codec/av1/{helpers,parser}.rs`. `round2signed_i64` preserves the intermediate product through rounding; `clip3_i64` applies AV1 bounds before a checked i32 conversion. Gamma and delta use checked i64 arithmetic, yielding a typed parse error rather than a panic for a theoretical out-of-range intermediate. The gamma lower Clip3 bound is corrected to -32768 from -32678. No yt-dlp, player-core, decoder-thread, renderer or public API changed.

The code follows AV1 Bitstream & Decoding Process Specification §7.11.3.6 and FFmpeg's `get_shear_params_valid`, which use wide intermediates for v/w. Rust Reference evidence was consulted for checked integer overflow and narrowing semantics.

## Regression contract

Parser-level regressions in `cros-codecs-patch/src/codec/av1/parser.rs` cover:
- a valid RotZoom transform whose `w * div_factor` is -4294967296, previously an i32 panic, now accepted;
- an invalid transform that premature i64-to-i32 truncation previously marked valid, now correctly rejected.

`docs/dependency-patches.toml` records AV1 global-motion arithmetic as an owned cros-codecs patch area and requires AV1 Main global-motion VA-API/DMA-BUF media coverage before patch removal.

## Validation

- `cargo +1.96.0 test --manifest-path crates/cros-codecs-patch/Cargo.toml --locked`: 60/60.
- `cargo +1.96.0 test -p video-vaapi --locked`: 160/160.
- `cargo +1.96.0 clippy -p video-vaapi --all-targets -- -D warnings`: PASS.
- `cargo +1.96.0 check --workspace --locked`: PASS.
- `python3 scripts/check-dependency-patches.py`: PASS.
- Fresh Fastiplayer VA-API hardware playback used the same locally downloaded 30-second source fragment at `/tmp/fastiplayer-JrT1PjOjOjc-av1-30s.mp4`. A 12-second run selected `vaapi-dmabuf-wgpu` and `VA-API AV1`, emitted 697 `video frame submitted to renderer` markers, and had zero panic/overflow/fatal/disconnected markers. It crosses the prior ~6.6-second failure point. Evidence: `/tmp/fastiplayer-av1-fix-hw/hardware-render-final.log`.

Direct strict Clippy for the entire standalone cros-codecs fork remains blocked by 42 pre-existing lint diagnostics in unrelated upstream files; the changed helpers/parser lines emit none. Do not mix remediation of that upstream lint baseline with this AV1 behavior fix.