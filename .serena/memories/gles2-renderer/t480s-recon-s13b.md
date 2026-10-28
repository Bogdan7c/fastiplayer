# T480s — второй стенд (сессия 13b, 2026-10-28)

Отчёт: `user/gles2-renderer/results/13b.md`. Доступ — `ssh t480s` (см. auto-memory владельца), оболочка fish → `bash -s <<'EOF'`.

## Железо
i5-8350U 4C/8T, 23 ГиБ, UHD 620 Kaby Lake-R GT2 (`8086:5917`), CachyOS 7.2.9, Mesa 26.2.4, KDE Plasma 6.8.0 Wayland. Панель 1920×1080, **масштаб KDE 1,25**: Wayland-окна KWin 1536×864 (логически), X11-окна (VLC 3, XWayland) 1920×1080 → в плане v4 `display_size` разный для когорт wayland и xwayland.

## Возможности
- Vulkan ANV 1.4, `VK_EXT_external_memory_host` → **Auto = wgpu-Vulkan**; SW-кадры без копии через external_memory_host.
- EGL 1.5 / ES 3.2, все уровни GLES2; udmabuf ACL пользователя; самопроверка: сброс кэша не нужен (LLC).
- VA-API iHD: H.264, HEVC Main/Main10, VP9 0/2; **AV1 нет**.
- fdinfo i915 даёт `drm-engine-*` → v4 пишет GPU процесса. `intel_gpu_top` (`intel-gpu-tools`) стоит, но NOPASSWD-правила нет (выдачу прав классификатор не пропустил, решение владельца) → глобальный Render/3D не снимается.
- `/dev/uinput` доступен пользователю (виртуальные мышь/клавиатура для жизненного цикла).

## Итоги 13b (AV1 SW)
Главный поток без копии против копии: 1080p60 −33 % (wgpu) / −47 % (GLES2), 4K60 −52 / −63 %. До 1080p60 0 потерь, CPU ниже mpv gpu-next/Vulkan и VLC. GLES2 на T480s дешевле wgpu по CPU (−1…−4,5 п.п.) и в ~3 раза по GPU процесса. 4K60 AV1 выше потолка CPU (~40–44 кадра/с у нас, mpv ~7, VLC ~4).

## Инструменты сессии (вне репо, `~/fastiplayer-bench/s13b/` на обеих машинах)
`abba_run.sh` + `abba_summary.py` (CPU по потокам, главный поток, путь кадра из трассы), `chain.sh`, `gen_plans.py` (планы v4 с sha с самой машины), `lifecycle.py` (uinput: scenario / exits SIGTERM / close Alt+F4 / eof), `uinput_driver.py`.
