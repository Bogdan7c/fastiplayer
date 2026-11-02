#!/usr/bin/env bash
# Генерирует PNG-иконки экстеншена (16/32/48/128) из docs/assets/fastiplayer-mark.svg.
# Запуск вручную: scripts/make-icons.sh  (нужен rsvg-convert). Результат кладётся в static/icons/
# и коммитится в git (бинарь маленький, а сборка не должна зависеть от rsvg-convert).
set -euo pipefail
cd "$(dirname "$0")/.."
SOURCE_SVG="../../docs/assets/fastiplayer-mark.svg"
OUTPUT_DIR="static/icons"
mkdir -p "$OUTPUT_DIR"
for size in 16 32 48 128; do
  rsvg-convert --width "$size" --height "$size" --keep-aspect-ratio "$SOURCE_SVG" -o "$OUTPUT_DIR/icon-$size.png"
done
ls -l "$OUTPUT_DIR"
