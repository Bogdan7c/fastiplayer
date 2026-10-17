// fastiplayer patch: раскладка AC-3 / E-AC-3 из `dac3` / `dec3` (ETSI TS 102 366, Annex F).
//
// Для sample entry `ac-3` / `ec-3` поле `ChannelCount` по спецификации «игнорируется»
// (ffmpeg пишет туда 2), поэтому upstream оставлял `channels = None`: MP4/MOV-дорожка
// AC-3 5.1 приходила в плеер без числа каналов (а MKV — с ним). Настоящая раскладка
// основной программы лежит в `AC3SpecificBox` / `EC3SpecificBox`: `acmod` + `lfeon`.
//
// Позиции совпадают с тем, как их понимает FFmpeg и декодер плеера: пара Ls/Rs →
// SIDE_LEFT/SIDE_RIGHT (5.1 = `5.1(side)`), одиночный surround → REAR_CENTER.

use symphonia_core::audio::{Channels, Position};

/// Длина `AC3SpecificBox`: fscod(2) bsid(5) bsmod(3) acmod(3) lfeon(1) bit_rate_code(5) reserved(5).
const DAC3_PAYLOAD_BYTES: usize = 3;
/// Минимальная длина `EC3SpecificBox` с одним независимым substream-ом:
/// data_rate(13) num_ind_sub(3) + fscod(2) bsid(5) reserved(1) asvc(1) bsmod(3)
/// acmod(3) lfeon(1) reserved(3) num_dep_sub(4) + chan_loc(9) | reserved(1).
const DEC3_MIN_PAYLOAD_BYTES: usize = 5;

/// Раскладка основной программы из `AC3SpecificBox`.
///
/// `None` — box короче спецификации: число каналов остаётся неизвестным, как и до
/// патча (декодер всё равно публикует настоящий формат по битовому потоку).
pub(crate) fn ac3_specific_box_channels(payload: &[u8]) -> Option<Channels> {
    let bytes: [u8; DAC3_PAYLOAD_BYTES] = payload.get(..DAC3_PAYLOAD_BYTES)?.try_into().ok()?;
    // Биты 10..13 (acmod) и 13 (lfeon), считая от старшего бита первого байта.
    let packed = u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2]);
    let acmod = ((packed >> 11) & 0b111) as u8;
    let lfeon = (packed >> 10) & 1 == 1;
    Some(program_channels(acmod, lfeon))
}

/// Раскладка основной программы (независимый substream 0) из `EC3SpecificBox`.
///
/// Только substream 0: декодер плеера играет именно основную программу. Если у неё
/// есть dependent substream-ы (`num_dep_sub > 0`, например 7.1), итоговая раскладка
/// зависит от `chan_loc`, порядок бит которого реализации трактуют по-разному, —
/// число каналов здесь не угадывается (`None`), его публикует декодер по первому кадру.
pub(crate) fn ec3_specific_box_channels(payload: &[u8]) -> Option<Channels> {
    let bytes: [u8; DEC3_MIN_PAYLOAD_BYTES] =
        payload.get(..DEC3_MIN_PAYLOAD_BYTES)?.try_into().ok()?;
    // Байты 2..4 — первые 24 бита описания substream-а 0 (после data_rate/num_ind_sub).
    let substream = u32::from(bytes[2]) << 16 | u32::from(bytes[3]) << 8 | u32::from(bytes[4]);
    let acmod = ((substream >> 9) & 0b111) as u8;
    let lfeon = (substream >> 8) & 1 == 1;
    let num_dep_sub = (substream >> 1) & 0b1111;
    if num_dep_sub > 0 {
        return None;
    }
    Some(program_channels(acmod, lfeon))
}

/// `acmod` (Table 5.8 ETSI TS 102 366) + LFE → позиции Symphonia.
fn program_channels(acmod: u8, lfeon: bool) -> Channels {
    let front_pair = Position::FRONT_LEFT | Position::FRONT_RIGHT;
    let side_pair = Position::SIDE_LEFT | Position::SIDE_RIGHT;
    let main = match acmod {
        // 1+1 (dual mono) декодируется как два независимых канала L/R.
        0 | 2 => front_pair,
        1 => Position::FRONT_CENTER,
        3 => front_pair | Position::FRONT_CENTER,
        4 => front_pair | Position::REAR_CENTER,
        5 => front_pair | Position::FRONT_CENTER | Position::REAR_CENTER,
        6 => front_pair | side_pair,
        // acmod занимает 3 бита: остаётся только 7 = 3/2.
        _ => front_pair | Position::FRONT_CENTER | side_pair,
    };
    let positions = if lfeon { main | Position::LFE1 } else { main };
    Channels::Positioned(positions)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Байты `dac3`, которые ffmpeg пишет для 5.1(side) 48 кГц 256k: acmod 7, lfeon 1.
    const FFMPEG_DAC3_5P1: [u8; 3] = [0x10, 0x3d, 0x80];

    fn count(channels: Option<Channels>) -> Option<usize> {
        channels.map(|channels| channels.count())
    }

    #[test]
    fn dac3_surround_publishes_six_side_channels() {
        let channels = ac3_specific_box_channels(&FFMPEG_DAC3_5P1);
        assert_eq!(
            channels,
            Some(Channels::Positioned(
                Position::FRONT_LEFT
                    | Position::FRONT_RIGHT
                    | Position::FRONT_CENTER
                    | Position::LFE1
                    | Position::SIDE_LEFT
                    | Position::SIDE_RIGHT
            ))
        );
    }

    #[test]
    fn every_acmod_maps_to_its_channel_count() {
        let expected = [2, 1, 2, 3, 3, 4, 4, 5];
        for (acmod, main_channels) in expected.into_iter().enumerate() {
            let acmod = acmod as u8;
            assert_eq!(program_channels(acmod, false).count(), main_channels);
            assert_eq!(program_channels(acmod, true).count(), main_channels + 1);
        }
    }

    #[test]
    fn short_boxes_leave_channels_unknown() {
        assert_eq!(ac3_specific_box_channels(&FFMPEG_DAC3_5P1[..2]), None);
        assert_eq!(ec3_specific_box_channels(&[0; 4]), None);
    }

    #[test]
    fn dec3_main_program_without_dependents_is_published() {
        // data_rate=256, num_ind_sub=0 | fscod 0 bsid 16 | acmod 2 lfeon 0, num_dep_sub 0.
        let stereo_lfe_less = [0x08, 0x00, 0x20, 0x04, 0x00];
        assert_eq!(count(ec3_specific_box_channels(&stereo_lfe_less)), Some(2));
        // Байты, которые ffmpeg пишет для E-AC-3 5.1(side): acmod 7 lfeon 1.
        let surround = [0x08, 0x00, 0x20, 0x0f, 0x00];
        assert_eq!(count(ec3_specific_box_channels(&surround)), Some(6));
    }

    #[test]
    fn dec3_with_dependent_substreams_is_not_guessed() {
        // Тот же 5.1, но num_dep_sub = 1 (+ chan_loc): итог задаёт декодер.
        let with_dependent = [0x08, 0x00, 0x20, 0x0f, 0x02, 0x00];
        assert_eq!(ec3_specific_box_channels(&with_dependent), None);
    }
}
