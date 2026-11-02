#!/usr/bin/env bash
# Генерирует тестовый ролик для живой проверки: 20 с, 854x480, 25 к/с, H.264 + AAC,
# два фрагментированных MP4 (видео и звук отдельно — как два SourceBuffer у MSE-страницы).
# Использование: make-media.sh <папка-вывода>
# Файлы в git не кладём (бинарные): они воссоздаются при первом запуске.
set -euo pipefail

OUT_DIR="${1:?usage: make-media.sh <output-dir>}"
mkdir -p "$OUT_DIR"

DURATION_S=20
FRAME_RATE=25
# Ключевой кадр раз в секунду: именно по ним режутся фрагменты (moof), как у реальных MSE-потоков.
KEYFRAME_INTERVAL=$FRAME_RATE
# Путь к шрифту для выжженного номера кадра; без drawtext/шрифта лучше упасть, чем получить немой ролик.
FONT_FILE="$(fc-match -f '%{file}' monospace)"
[ -f "$FONT_FILE" ] || { echo "make-media: no monospace font found" >&2; exit 1; }

# bitexact + один поток кодера + вырезанные метаданные: один и тот же ролик при каждой генерации.
GLOBAL_FLAGS=(-hide_banner -loglevel error -y)
OUTPUT_FLAGS=(-fflags +bitexact -map_metadata -1 -threads 1)   # идут ПОСЛЕ -i: это опции выхода
FRAGMENT_FLAGS=(-movflags frag_keyframe+empty_moov+default_base_moof -flags +bitexact)

# Видео: testsrc2 + крупно номер кадра и время (по ним видно, что плеер не теряет кадры).
ffmpeg "${GLOBAL_FLAGS[@]}" \
  -f lavfi -i "testsrc2=size=854x480:rate=$FRAME_RATE:duration=$DURATION_S" \
  "${OUTPUT_FLAGS[@]}" -vf "drawtext=fontfile='$FONT_FILE':text='%{eif\\:n\\:d\\:5}  %{pts\\:hms}':fontsize=64:fontcolor=white:box=1:boxcolor=black@0.6:x=(w-text_w)/2:y=h-90" \
  -an -c:v libx264 -profile:v baseline -level 3.1 -pix_fmt yuv420p -b:v 700k \
  -g "$KEYFRAME_INTERVAL" -keyint_min "$KEYFRAME_INTERVAL" -sc_threshold 0 \
  "${FRAGMENT_FLAGS[@]}" "$OUT_DIR/video.mp4"

# Звук: синус 440 Гц, стерео 48 кГц, AAC-LC; фрагмент раз в секунду.
ffmpeg "${GLOBAL_FLAGS[@]}" \
  -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=$DURATION_S" \
  "${OUTPUT_FLAGS[@]}" -vn -ac 2 -c:a aac -b:a 96k \
  -frag_duration 1000000 "${FRAGMENT_FLAGS[@]}" "$OUT_DIR/audio.mp4"

echo "make-media: wrote $OUT_DIR/video.mp4 and $OUT_DIR/audio.mp4"
