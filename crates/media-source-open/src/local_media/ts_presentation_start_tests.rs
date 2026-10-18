//! Старт шкалы MPEG-TS доходит до потребителей через production local registry.
//!
//! Фикстура `ts_fixture` начинается с PTS 1,0 с; плеер показывает сырое время
//! контейнера, поэтому внешним субтитрам нужен этот старт (`presentation_start`).

use std::time::Duration;

use super::ac3_elementary_tests::open_local;
use super::ts_fixture::mpeg_ts_h264_aac_bytes;

#[test]
fn local_ts_with_nonzero_first_pts_publishes_container_presentation_start() {
    let demuxer = open_local("recording.ts", &mpeg_ts_h264_aac_bytes());

    let start = demuxer
        .media_metadata()
        .and_then(|metadata| metadata.container)
        .and_then(|container| container.presentation_start);

    assert_eq!(start, Some(Duration::from_secs(1)));
}
