//! Тесты декодера на потоках, собранных `PgsStreamBuilder`: от байтов блоков до пикселей.

use subtitle_core::{PictureForcedFlag, SubtitleTime};

use super::*;
use crate::pcs::{CompositionObject, CropRect};
use crate::picture_decode::{PictureDecodeLimits, decode_picture_rgba};
use crate::stream_builder::{
    DisplaySetSpec, ObjectImageSpec, PaletteColorSpec, PaletteSpec, PgsStreamBuilder,
    encode_display_set_segments,
};
use crate::wds::WindowDefinition;

const LIMITS: PgsDecoderLimits = PgsDecoderLimits {
    max_object_rle_bytes: 1 << 20,
    max_epoch_rle_bytes: 4 << 20,
};
const PIXEL_LIMITS: PictureDecodeLimits = PictureDecodeLimits {
    max_picture_pixels: 1 << 20,
};

fn at(millis: i64) -> SubtitleTime {
    SubtitleTime::from_millis(millis)
}

fn palette(id: u8, colors: &[(u8, [u8; 4])]) -> PaletteSpec {
    PaletteSpec {
        palette_id: id,
        version: 0,
        colors: colors
            .iter()
            .map(|&(index, rgba)| PaletteColorSpec { index, rgba })
            .collect(),
    }
}

fn image(id: u16, width: u16, height: u16, indices: Vec<u8>) -> ObjectImageSpec {
    ObjectImageSpec {
        object_id: id,
        width,
        height,
        indices,
        max_rle_bytes_per_fragment: None,
    }
}

fn placed(id: u16, x: u16, y: u16) -> CompositionObject {
    CompositionObject {
        object_id: id,
        window_id: 0,
        forced: PictureForcedFlag::Regular,
        x,
        y,
        crop: None,
    }
}

fn window(x: u16, y: u16, width: u16, height: u16) -> WindowDefinition {
    WindowDefinition {
        window_id: 0,
        x,
        y,
        width,
        height,
    }
}

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];

/// Epoch Start 1920x1080: картинка 4x2 (красный/зелёный) в точке (100, 50).
fn epoch_start() -> DisplaySetSpec {
    DisplaySetSpec::new(1920, 1080, CompositionState::EpochStart)
        .with_window(window(0, 0, 1920, 1080))
        .with_palette(palette(0, &[(1, RED), (2, GREEN)]))
        .with_image(image(1, 4, 2, vec![1, 1, 2, 2, 2, 2, 1, 1]))
        .with_composition_object(placed(1, 100, 50))
}

fn clear_set(state: CompositionState) -> DisplaySetSpec {
    DisplaySetSpec::new(1920, 1080, state)
}

fn feed(decoder: &mut PgsTrackDecoder, builder: &PgsStreamBuilder) -> Vec<PgsComposition> {
    builder
        .block_payloads()
        .iter()
        .flat_map(|(time, payload)| decoder.push_block(*time, payload))
        .collect()
}

fn pixel_of(composition: &PgsComposition, cue_index: usize, x: u32, y: u32) -> [u8; 4] {
    let decoded = decode_picture_rgba(&composition.cues[cue_index], PIXEL_LIMITS).expect("decode");
    decoded.image.pixel(x, y).expect("pixel inside region")
}

fn close(actual: [u8; 4], expected: [u8; 4]) {
    for channel in 0..4 {
        assert!(
            actual[channel].abs_diff(expected[channel]) <= 3,
            "{actual:?} vs {expected:?}"
        );
    }
}

#[test]
fn epoch_start_produces_cue_with_expected_pixels_position_and_time() {
    let mut builder = PgsStreamBuilder::new();
    builder.push_display_set(at(500), &epoch_start());
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    let compositions = feed(&mut decoder, &builder);
    assert_eq!(compositions.len(), 1);
    let composition = &compositions[0];
    assert_eq!(composition.start, at(500));
    assert_eq!(composition.cues.len(), 1);
    let cue = &composition.cues[0];
    assert_eq!(cue.origin().x, 100);
    assert_eq!(cue.origin().y, 50);
    assert_eq!(cue.reference_size().width().get(), 1920);
    assert_eq!(cue.forced(), PictureForcedFlag::Regular);
    close(pixel_of(composition, 0, 0, 0), RED);
    close(pixel_of(composition, 0, 2, 0), GREEN);
    close(pixel_of(composition, 0, 3, 1), RED);
    close(pixel_of(composition, 0, 0, 1), GREEN);
}

#[test]
fn hd_and_sd_palettes_use_different_matrices() {
    let sd = DisplaySetSpec::new(720, 480, CompositionState::EpochStart)
        .with_palette(palette(0, &[(1, RED)]))
        .with_image(image(1, 1, 1, vec![1]))
        .with_composition_object(placed(1, 0, 0));
    let mut builder = PgsStreamBuilder::new();
    builder.push_display_set(at(0), &sd);
    // Те же байты YCbCr, вручную выданные как HD-кадр, дают другой RGB: проверяем разницу.
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    let sd_out = feed(&mut decoder, &builder);
    close(pixel_of(&sd_out[0], 0, 0, 0), RED);
    let hd = DisplaySetSpec::new(1280, 720, CompositionState::EpochStart)
        .with_palette(palette(0, &[(1, RED)]))
        .with_image(image(1, 1, 1, vec![1]))
        .with_composition_object(placed(1, 0, 0));
    let mut hd_builder = PgsStreamBuilder::new();
    hd_builder.push_display_set(at(0), &hd);
    let hd_out = feed(&mut PgsTrackDecoder::new(LIMITS), &hd_builder);
    close(pixel_of(&hd_out[0], 0, 0, 0), RED);
    // Кросс-проверка: байты PDS для SD и HD действительно различаются (разные Y/Cb/Cr красного).
    let sd_bytes = builder.block_payloads()[0].1.clone();
    let hd_bytes = hd_builder.block_payloads()[0].1.clone();
    assert_ne!(sd_bytes, hd_bytes);
}

#[test]
fn clear_composition_has_no_cues() {
    let mut builder = PgsStreamBuilder::new();
    builder
        .push_display_set(at(500), &epoch_start())
        .push_display_set(at(1_500), &clear_set(CompositionState::Normal));
    let compositions = feed(&mut PgsTrackDecoder::new(LIMITS), &builder);
    assert_eq!(compositions.len(), 2);
    assert_eq!(compositions[1].start, at(1_500));
    assert!(compositions[1].cues.is_empty());
}

#[test]
fn multi_fragment_object_is_reassembled_into_the_same_pixels() {
    let indices: Vec<u8> = (0..64).map(|n| if n % 3 == 0 { 1 } else { 2 }).collect();
    let mut fragmented = image(1, 8, 8, indices.clone());
    fragmented.max_rle_bytes_per_fragment = Some(7);
    let spec = |image: ObjectImageSpec| {
        DisplaySetSpec::new(1920, 1080, CompositionState::EpochStart)
            .with_palette(palette(0, &[(1, RED), (2, GREEN)]))
            .with_image(image)
            .with_composition_object(placed(1, 0, 0))
    };
    let whole_segments = encode_display_set_segments(&spec(image(1, 8, 8, indices.clone())), 0);
    let fragment_segments = encode_display_set_segments(&spec(fragmented), 0);
    assert!(fragment_segments.len() > whole_segments.len() + 2);

    let mut builder = PgsStreamBuilder::new();
    builder.push_display_set(
        at(0),
        &spec({
            let mut multi = image(1, 8, 8, indices);
            multi.max_rle_bytes_per_fragment = Some(7);
            multi
        }),
    );
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    let out = feed(&mut decoder, &builder);
    assert_eq!(out.len(), 1);
    close(pixel_of(&out[0], 0, 0, 0), RED);
    close(pixel_of(&out[0], 0, 1, 0), GREEN);
    close(pixel_of(&out[0], 0, 3, 0), RED);
    assert_eq!(decoder.statistics().object_length_mismatches, 0);
    assert_eq!(decoder.statistics().incomplete_objects, 0);
}

#[test]
fn palette_only_update_reuses_objects_with_new_colours() {
    let update = DisplaySetSpec::new(1920, 1080, CompositionState::Normal)
        .palette_update_only()
        .with_palette_id(1)
        .with_palette(palette(1, &[(1, GREEN), (2, RED)]));
    let mut builder = PgsStreamBuilder::new();
    builder
        .push_display_set(at(0), &epoch_start())
        .push_display_set(at(1_000), &update);
    let out = feed(&mut PgsTrackDecoder::new(LIMITS), &builder);
    assert_eq!(out.len(), 2);
    assert!(Arc::ptr_eq(
        out[0].cues[0].picture(),
        out[1].cues[0].picture()
    ));
    close(pixel_of(&out[0], 0, 0, 0), RED);
    close(pixel_of(&out[1], 0, 0, 0), GREEN);
    close(pixel_of(&out[1], 0, 2, 0), RED);
}

#[test]
fn normal_set_can_reference_object_and_palette_from_earlier_set() {
    let normal = DisplaySetSpec::new(1920, 1080, CompositionState::Normal)
        .with_composition_object(placed(1, 300, 400));
    let mut builder = PgsStreamBuilder::new();
    builder
        .push_display_set(at(0), &epoch_start())
        .push_display_set(at(1_000), &clear_set(CompositionState::Normal))
        .push_display_set(at(2_000), &normal);
    let out = feed(&mut PgsTrackDecoder::new(LIMITS), &builder);
    assert_eq!(out.len(), 3);
    assert_eq!(out[2].cues[0].origin().x, 300);
    close(pixel_of(&out[2], 0, 0, 0), RED);
}

#[test]
fn normal_set_with_missing_object_clears_the_screen_is_counted_and_next_set_works() {
    let broken = DisplaySetSpec::new(1920, 1080, CompositionState::Normal)
        .with_composition_object(placed(9, 0, 0));
    let mut builder = PgsStreamBuilder::new();
    builder
        .push_display_set(at(0), &epoch_start())
        .push_display_set(at(1_000), &broken)
        .push_display_set(at(2_000), &clear_set(CompositionState::Normal));
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    let out = feed(&mut decoder, &builder);
    assert_eq!(
        out.iter().map(|c| c.start).collect::<Vec<_>>(),
        [at(0), at(1_000), at(2_000)]
    );
    // Несобираемая композиция гасит прошлую картинку, а не оставляет её висеть.
    assert!(out[1].cues.is_empty());
    assert_eq!(
        decoder.statistics().compositions_skipped_missing_resource,
        1
    );
}

#[test]
fn epoch_start_resets_previous_objects() {
    let fresh_epoch = DisplaySetSpec::new(1920, 1080, CompositionState::EpochStart);
    let normal = DisplaySetSpec::new(1920, 1080, CompositionState::Normal)
        .with_composition_object(placed(1, 0, 0));
    let mut builder = PgsStreamBuilder::new();
    builder
        .push_display_set(at(0), &epoch_start())
        .push_display_set(at(1_000), &fresh_epoch)
        .push_display_set(at(2_000), &normal);
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    let out = feed(&mut decoder, &builder);
    // Объект 1 из прошлой эпохи забыт: третья композиция превращается в «очистку».
    assert_eq!(out.len(), 3);
    assert!(out[2].cues.is_empty());
    assert_eq!(
        decoder.statistics().compositions_skipped_missing_resource,
        1
    );
}

#[test]
fn normal_sets_are_ignored_after_discontinuity_until_acquisition_point() {
    let acquisition = DisplaySetSpec::new(1920, 1080, CompositionState::AcquisitionPoint)
        .with_palette(palette(0, &[(1, GREEN)]))
        .with_image(image(1, 2, 1, vec![1, 1]))
        .with_composition_object(placed(1, 10, 20));
    let normal = DisplaySetSpec::new(1920, 1080, CompositionState::Normal)
        .with_composition_object(placed(1, 0, 0));
    let mut builder = PgsStreamBuilder::new();
    builder.push_display_set(at(0), &epoch_start());
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    assert_eq!(feed(&mut decoder, &builder).len(), 1);

    decoder.reset_after_discontinuity();
    let mut after = PgsStreamBuilder::new();
    after
        .push_display_set(at(5_000), &normal)
        .push_display_set(at(6_000), &acquisition)
        .push_display_set(at(7_000), &normal);
    let out = feed(&mut decoder, &after);
    assert_eq!(
        out.iter().map(|c| c.start).collect::<Vec<_>>(),
        [at(6_000), at(7_000)]
    );
    assert_eq!(decoder.statistics().sets_ignored_until_entry_point, 1);
    assert_eq!(decoder.statistics().discontinuities, 1);
    close(pixel_of(&out[0], 0, 1, 0), GREEN);
    // Normal на 7 с использовал объект из точки входа.
    assert_eq!(out[1].cues[0].origin().x, 0);
}

#[test]
fn stream_must_start_at_an_entry_point() {
    let normal = DisplaySetSpec::new(1920, 1080, CompositionState::Normal)
        .with_composition_object(placed(1, 0, 0));
    let mut builder = PgsStreamBuilder::new();
    builder.push_display_set(at(0), &normal);
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    assert!(feed(&mut decoder, &builder).is_empty());
    assert_eq!(decoder.statistics().sets_ignored_until_entry_point, 1);
}

#[test]
fn new_pcs_before_end_closes_the_previous_set_implicitly() {
    let mut first = encode_display_set_segments(&epoch_start(), 0);
    first.pop(); // Убираем END первого набора.
    let second = encode_display_set_segments(&clear_set(CompositionState::Normal), 1);
    let mut payload = first.concat();
    payload.extend(second.concat());
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    let out = decoder.push_block(at(100), &payload);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].cues.len(), 1);
    assert!(out[1].cues.is_empty());
    assert_eq!(decoder.statistics().implicit_end_sets, 1);
}

#[test]
fn broken_segment_is_skipped_with_counter_and_next_set_still_decodes() {
    let mut garbage_set = encode_display_set_segments(&epoch_start(), 0);
    // Портим PDS: оставляем один байт (нет даже версии палитры) — сегмент не разобрать.
    let pds_position = garbage_set
        .iter()
        .position(|segment| segment[0] == 0x14)
        .expect("builder emits a palette");
    garbage_set[pds_position] = vec![0x14, 0x00, 0x01, 0];
    let mut builder = PgsStreamBuilder::new();
    builder.push_display_set(at(2_000), &epoch_start());
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    let first = decoder.push_block(at(0), &garbage_set.concat());
    // Без палитры композиция пустая («очистка»), но дорожка жива.
    assert_eq!(first.len(), 1);
    assert!(first[0].cues.is_empty());
    assert_eq!(decoder.statistics().malformed_segments, 1);
    assert_eq!(
        decoder.statistics().compositions_skipped_missing_resource,
        1
    );
    let out = feed(&mut decoder, &builder);
    assert_eq!(out.len(), 1);
    close(pixel_of(&out[0], 0, 0, 0), RED);
}

#[test]
fn truncated_block_and_unknown_segments_are_counted() {
    let mut payload = vec![0x7F, 0x00, 0x01, 0xAA];
    payload.extend_from_slice(&[0x16, 0x00, 0x20, 1, 2]); // Обрыв посреди сегмента.
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    assert!(decoder.push_block(at(0), &payload).is_empty());
    assert_eq!(decoder.statistics().unknown_segments, 1);
    assert_eq!(decoder.statistics().blocks_with_framing_errors, 1);
    // Сегмент без набора.
    assert!(decoder.push_block(at(1), &[0x15, 0, 0]).is_empty());
    assert_eq!(decoder.statistics().orphan_segments, 1);
    // END без набора тоже считается.
    assert!(decoder.push_block(at(2), &[0x80, 0, 0]).is_empty());
    assert_eq!(decoder.statistics().orphan_segments, 2);
}

#[test]
fn crop_and_window_clip_define_visible_region_and_origin() {
    // Картинка 8x4. Обрезка (2,1)-(6x3) показывается с позиции (100, 50).
    // Окно начинается с (102, 51): отсекает слева 2 пикселя и сверху 1.
    let mut object = placed(1, 100, 50);
    object.crop = Some(CropRect {
        x: 2,
        y: 1,
        width: 6,
        height: 3,
    });
    let spec = DisplaySetSpec::new(1920, 1080, CompositionState::EpochStart)
        .with_window(window(102, 51, 1000, 1000))
        .with_palette(palette(0, &[(1, RED), (2, GREEN)]))
        .with_image(image(
            1,
            8,
            4,
            (0..32).map(|n| if n % 8 < 4 { 1 } else { 2 }).collect(),
        ))
        .with_composition_object(object);
    let mut builder = PgsStreamBuilder::new();
    builder.push_display_set(at(0), &spec);
    let out = feed(&mut PgsTrackDecoder::new(LIMITS), &builder);
    let cue = &out[0].cues[0];
    assert_eq!((cue.origin().x, cue.origin().y), (102, 51));
    let region = cue.visible_region();
    assert_eq!((region.x(), region.y()), (4, 2));
    assert_eq!((region.width().get(), region.height().get()), (4, 2));
    // Левый верхний видимый пиксель — картинка (4, 2): колонка 4 -> зелёный.
    close(pixel_of(&out[0], 0, 0, 0), GREEN);
}

#[test]
fn object_fully_outside_window_is_dropped_and_counted() {
    let spec = DisplaySetSpec::new(1920, 1080, CompositionState::EpochStart)
        .with_window(window(0, 0, 50, 50))
        .with_palette(palette(0, &[(1, RED)]))
        .with_image(image(1, 4, 2, vec![1; 8]))
        .with_composition_object(placed(1, 500, 500));
    let mut builder = PgsStreamBuilder::new();
    builder.push_display_set(at(0), &spec);
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    let out = feed(&mut decoder, &builder);
    assert!(out[0].cues.is_empty());
    assert_eq!(decoder.statistics().objects_dropped_no_visible_area, 1);
}

#[test]
fn forced_flag_is_kept_per_object() {
    let mut forced = placed(2, 0, 100);
    forced.forced = PictureForcedFlag::Forced;
    let spec = DisplaySetSpec::new(1920, 1080, CompositionState::EpochStart)
        .with_palette(palette(0, &[(1, RED)]))
        .with_image(image(1, 2, 2, vec![1; 4]))
        .with_image(image(2, 2, 2, vec![1; 4]))
        .with_composition_object(placed(1, 0, 0))
        .with_composition_object(forced);
    let mut builder = PgsStreamBuilder::new();
    builder.push_display_set(at(0), &spec);
    let out = feed(&mut PgsTrackDecoder::new(LIMITS), &builder);
    let flags: Vec<_> = out[0].cues.iter().map(|cue| cue.forced()).collect();
    assert_eq!(
        flags,
        [PictureForcedFlag::Regular, PictureForcedFlag::Forced]
    );
}

#[test]
fn object_over_size_limit_is_rejected_and_composition_skipped() {
    let tiny = PgsDecoderLimits {
        max_object_rle_bytes: 4,
        max_epoch_rle_bytes: 1 << 20,
    };
    let mut builder = PgsStreamBuilder::new();
    builder.push_display_set(at(0), &epoch_start());
    let mut decoder = PgsTrackDecoder::new(tiny);
    let out = feed(&mut decoder, &builder);
    assert_eq!(out.len(), 1);
    assert!(out[0].cues.is_empty());
    assert_eq!(decoder.statistics().objects_rejected_over_limit, 1);
    assert_eq!(
        decoder.statistics().compositions_skipped_missing_resource,
        1
    );
}

#[test]
fn block_with_several_display_sets_and_sup_header_is_handled() {
    let mut payload = b"PG".to_vec();
    payload.extend_from_slice(&[0; 11]);
    payload.extend(encode_display_set_segments(&epoch_start(), 0).concat());
    payload.extend(encode_display_set_segments(&clear_set(CompositionState::Normal), 1).concat());
    let out = PgsTrackDecoder::new(LIMITS).push_block(at(0), &payload);
    assert_eq!(out.len(), 2);
}

#[test]
fn oversized_reference_frame_is_rejected_and_counted_while_full_hd_plays() {
    let oversized = DisplaySetSpec::new(8_000, 8_000, CompositionState::EpochStart)
        .with_palette(palette(0, &[(1, RED)]))
        .with_image(image(1, 2, 2, vec![1, 1, 1, 1]))
        .with_composition_object(placed(1, 0, 0));
    let mut builder = PgsStreamBuilder::new();
    builder
        .push_display_set(at(0), &oversized)
        .push_display_set(at(500), &epoch_start());
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    let compositions = feed(&mut decoder, &builder);
    assert_eq!(decoder.statistics().oversized_reference_frames, 1);
    // Только нормальный 1920x1080 набор дал композицию.
    assert_eq!(compositions.len(), 1);
    assert_eq!(compositions[0].start, at(500));
}

#[test]
fn object_larger_than_its_reference_frame_is_rejected_and_counted() {
    let hostile = DisplaySetSpec::new(64, 32, CompositionState::EpochStart)
        .with_palette(palette(0, &[(1, RED)]))
        .with_image(image(1, 65, 4, vec![1; 65 * 4]))
        .with_composition_object(placed(1, 0, 0));
    let mut builder = PgsStreamBuilder::new();
    builder.push_display_set(at(0), &hostile);
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    let compositions = feed(&mut decoder, &builder);
    assert_eq!(decoder.statistics().objects_larger_than_reference_frame, 1);
    // Объекта нет -> композиция несобираема -> «очистка», а не картинка.
    assert_eq!(compositions.len(), 1);
    assert!(compositions[0].cues.is_empty());
}

#[test]
fn palette_with_partial_trailing_entry_is_still_used_and_counted() {
    let mut segments = encode_display_set_segments(&epoch_start(), 0);
    let pds_position = segments
        .iter()
        .position(|segment| segment[0] == 0x14)
        .expect("builder emits a palette");
    // Дописываем 2 лишних байта к PDS (заголовок сегмента: тип + длина u16).
    let pds = &mut segments[pds_position];
    let new_length = u16::try_from(pds.len() - 3 + 2).expect("small");
    pds[1..3].copy_from_slice(&new_length.to_be_bytes());
    pds.extend_from_slice(&[9, 9]);
    let mut decoder = PgsTrackDecoder::new(LIMITS);
    let out = decoder.push_block(at(0), &segments.concat());
    assert_eq!(decoder.statistics().palettes_with_partial_trailing_entry, 1);
    assert_eq!(decoder.statistics().malformed_segments, 0);
    assert_eq!(out.len(), 1);
    close(pixel_of(&out[0], 0, 0, 0), RED);
}
