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

# (d) MKV: VP8 + ДВЕ аудиодорожки с разным кодеком/частотой/каналами/содержимым
# (фикстура для переключения аудиодорожки, сессия 07 плана субтитров):
#   дорожка 1: FLAC, 22050 Гц, стерео, синус 440 Гц, язык rus, название «Dub»;
#   дорожка 2: Opus, 48000 Гц, моно, синус 880 Гц, язык eng, название «Original».
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc=size=64x64:rate=10:duration=3" \
  -f lavfi -i "sine=frequency=440:sample_rate=22050:duration=3" \
  -f lavfi -i "sine=frequency=880:sample_rate=48000:duration=3" \
  -map 0:v -map 1:a -map 2:a \
  -c:v libvpx -b:v 40k -c:a:0 flac -ac:a:0 2 -c:a:1 libopus -b:a:1 16k -ac:a:1 1 \
  -metadata:s:v:0 language=eng -metadata:s:v:0 title="Main video" \
  -metadata:s:a:0 language=rus -metadata:s:a:0 title="Dub" \
  -metadata:s:a:1 language=eng -metadata:s:a:1 title="Original" \
  -disposition:a:0 default -disposition:a:1 0 \
  "$here/two-audio-tracks.mkv"

# (e) MKV: VP8 + ДВЕ аудиодорожки (как в (d)) + ДВЕ SRT-дорожки (как в (a)): «фильм с двумя
# озвучками и субтитрами» для теста памяти выбора (сессия 07 плана субтитров):
#   аудио 1: FLAC 22050 Гц стерео 440 Гц, rus «Dub» (default); аудио 2: Opus 48000 Гц моно 880 Гц, eng «Original»;
#   субтитры 1: eng «English» (default), реплики «Hello» 0,5-1,5 с и «Second line» 2,0-2,8 с;
#   субтитры 2: rus «Russian SDH» (forced + hearing_impaired).
ffmpeg -hide_banner -loglevel error -y \
  "${video[@]}" \
  -f lavfi -i "sine=frequency=440:sample_rate=22050:duration=3" \
  -f lavfi -i "sine=frequency=880:sample_rate=48000:duration=3" \
  -i "$work/eng.srt" -i "$work/rus.srt" \
  -map 0:v -map 1:a -map 2:a -map 3 -map 4 \
  -c:v libvpx -b:v 40k -c:a:0 flac -ac:a:0 2 -c:a:1 libopus -b:a:1 16k -ac:a:1 1 -c:s srt \
  -metadata:s:v:0 language=eng -metadata:s:v:0 title="Main video" \
  -metadata:s:a:0 language=rus -metadata:s:a:0 title="Dub" \
  -metadata:s:a:1 language=eng -metadata:s:a:1 title="Original" \
  -metadata:s:s:0 language=eng -metadata:s:s:0 title="English" \
  -metadata:s:s:1 language=rus -metadata:s:s:1 title="Russian SDH" \
  -disposition:a:0 default -disposition:a:1 0 \
  -disposition:s:0 default -disposition:s:1 forced+hearing_impaired \
  "$here/two-audio-two-srt.mkv"

# (f) MKV: только AAC-LC 5.1(side) с Program Config Element (channel_configuration = 0 в ASC):
# синус 880 Гц, 1 с, 48 кГц, ТОЛЬКО в канале FC. Для теста `aac_pce_playback.rs`.
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "sine=frequency=880:sample_rate=48000:duration=1" \
  -af "pan=5.1(side)|FC=c0" -c:a aac -b:a 96k \
  "$here/aac-pce-5p1.mkv"

# (g) MKV: низкий fps + звук, длинный для обычного (не короткого) старта. VP8 64x64, 10 fps,
# 8 с + Opus mono 48 кГц (синус 440 Гц). Для теста `startup_decode_ahead.rs`: frame-threaded
# декодеру нужно больше packets до первого кадра, чем помещается в окно decode-ahead.
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc=size=64x64:rate=10:duration=8" \
  -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=8" \
  -map 0:v -map 1:a -c:v libvpx -b:v 40k -g 20 -c:a libopus -b:a 8k -ac 1 \
  "$here/low-fps-vp8-opus.mkv"

# (h) MKV: VP8 + ASS-дорожка «слой 1» (сессия 08 плана субтитров). PlayRes 640x360, события
# одновременно с 1.0 до 3.0 с:
#   - знак `{\pos(160,60)\an5}SIGN` красным (стиль Sign, &H000000FF = RGB 255,0,0);
#   - две нижние реплики в одном месте (an2): «First bottom line» белым (Default) и
#     «Second bottom line» жёлтым (Second, RGB 255,255,0) - сами сталкиваются и встают друг над другом.
# Шрифты не вложены: тесты берут встроенный шрифт приложения (Arial -> запасной).
cat > "$work/layer1.ass" <<'ASS'
[Script Info]
ScriptType: v4.00+
PlayResX: 640
PlayResY: 360
WrapStyle: 0
ScaledBorderAndShadow: yes

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,36,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,20,1
Style: Second,Arial,36,&H0000FFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,20,1
Style: Sign,Arial,40,&H000000FF,&H000000FF,&H00000000,&H00000000,1,0,0,0,100,100,0,0,1,2,0,5,10,10,10,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:03.00,Sign,,0,0,0,,{\pos(160,60)\an5}SIGN
Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,First bottom line
Dialogue: 0,0:00:01.00,0:00:03.00,Second,,0,0,0,,Second bottom line
ASS

ffmpeg -hide_banner -loglevel error -y \
  "${video[@]}" -i "$work/layer1.ass" \
  -map 0:v -map 1 \
  -c:v libvpx -b:v 40k -c:s ass \
  -metadata:s:s:0 language=eng -metadata:s:s:0 title="Layer 1" \
  "$here/ass-layer1.mkv"

# (i) MKV: VP8 + PGS-дорожка (сессия 11 плана субтитров). Поток .sup пишет детерминированный
# пример `subtitle-pgs` (`write_test_sup`), ffmpeg только упаковывает его без перекодирования.
# `-copyts` обязателен: иначе ffmpeg сдвигает дорожку так, что первый набор оказывается на 0 с.
# Расположение и цвета объектов — в README.md. В MKV каждый набор отображения (PCS..END)
# лежит ОДНИМ блоком голых сегментов, длительности у блоков нет.
repo_root="$(cd "$here/../../../../.." && pwd)"
(cd "$repo_root" && cargo run -q -p subtitle-pgs --features test-support --example write_test_sup -- "$work/pgs.sup")
ffmpeg -hide_banner -loglevel error -y -copyts \
  "${video[@]}" -i "$work/pgs.sup" \
  -map 0:v -map 1 \
  -c:v libvpx -b:v 40k -c:s copy \
  -metadata:s:s:0 language=eng -metadata:s:s:0 title="PGS" \
  "$here/pgs-basic.mkv"
