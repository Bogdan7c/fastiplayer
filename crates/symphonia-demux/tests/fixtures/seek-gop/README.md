# seek-gop

`gop2s-bframes.mp4` — 20 с, H.264 High, 30 fps, B-frames (`-bf 2`), IDR ровно каждые 2.000 с
(0, 2, 4, …, 18 с), моно AAC. Нужен тесту `tests/mp4_decode_point_before_seek.rs`: у MP4 с
B-frames DTS keyframe-а меньше его PTS на ~67 мс, поэтому seek в окно перед keyframe-ом
находит keyframe, который показывается уже после цели. Перегенерация: `./generate.sh`.
