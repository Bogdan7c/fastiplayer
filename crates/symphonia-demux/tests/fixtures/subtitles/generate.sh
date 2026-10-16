#!/usr/bin/env bash
# Генерация фикстур субтитров для crates/symphonia-demux/tests/subtitle_tracks.rs.
#
# Запуск: crates/symphonia-demux/tests/fixtures/subtitles/generate.sh
# Требуется ffmpeg (проверено на n9.0.2). Результат детерминирован по содержимому, но
# не побитово (ffmpeg пишет версию и UID); поэтому файлы коммитятся, а тесты опираются
# только на известные тайминги и тексты реплик, перечисленные в README.md.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
font="$here/../../../../subtitle-raster/fonts/NotoSansHebrew-Regular.ttf"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Реплики: известные времена (мс) и тексты проверяются тестами.
cat > "$work/eng.srt" <<'SRT'
1
00:00:00,500 --> 00:00:01,500
Hello

2
00:00:02,000 --> 00:00:02,800
Second line
SRT
cat > "$work/rus.srt" <<'SRT'
1
00:00:00,500 --> 00:00:01,500
Привет

2
00:00:02,000 --> 00:00:02,800
Второй
SRT
cat > "$work/styled.ass" <<'ASS'
[Script Info]
ScriptType: v4.00+
PlayResX: 64
PlayResY: 64

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Noto Sans Hebrew,12,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,2,2,2,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:00.50,0:00:01.50,Default,,0,0,0,,Styled {\b1}line{\b0}
ASS

video=(-f lavfi -i "testsrc=size=64x64:rate=10:duration=3")

# (a) MKV: VP8 + Opus + две SRT-дорожки с разными языками/названиями/флагами.
ffmpeg -hide_banner -loglevel error -y \
  "${video[@]}" -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=3" \
  -i "$work/eng.srt" -i "$work/rus.srt" \
  -map 0:v -map 1:a -map 2 -map 3 \
  -c:v libvpx -b:v 40k -c:a libopus -b:a 8k -ac 1 -c:s srt \
  -metadata:s:v:0 language=eng -metadata:s:v:0 title="Main video" \
  -metadata:s:a:0 language=jpn -metadata:s:a:0 title="Japanese mono" \
  -metadata:s:s:0 language=eng -metadata:s:s:0 title="English" \
  -metadata:s:s:1 language=rus -metadata:s:s:1 title="Russian SDH" \
  -disposition:s:0 default -disposition:s:1 forced+hearing_impaired \
  "$here/two-srt-tracks.mkv"

# (b) MKV: VP8 + ASS-дорожка + вложенный шрифт (OFL, Noto Sans Hebrew).
ffmpeg -hide_banner -loglevel error -y \
  "${video[@]}" -i "$work/styled.ass" \
  -map 0:v -map 1 \
  -c:v libvpx -b:v 40k -c:s ass \
  -metadata:s:s:0 language=eng -metadata:s:s:0 title="Styled" \
  -attach "$font" -metadata:s:t:0 mimetype=font/ttf \
  "$here/ass-with-font.mkv"

# (c) MP4: H.264 + mov_text (язык mdhd = rus).
ffmpeg -hide_banner -loglevel error -y \
  "${video[@]}" -i "$work/rus.srt" \
  -map 0:v -map 1 \
  -c:v libx264 -preset ultrafast -crf 40 -pix_fmt yuv420p -c:s mov_text \
  -metadata:s:s:0 language=rus \
  "$here/mov-text-rus.mp4"
