//! Compatibility boundary для старого crate name `webm-demux`.
//!
//! Implementation переехал в `symphonia-demux`, потому что adapter открывает
//! все audio containers, которые поддерживает Symphonia, а не только WebM/MKV.
//! Этот crate остаётся на один transition PR, чтобы старые call sites могли
//! продолжить компилироваться через явный re-export без изменения поведения.

pub use symphonia_demux::*;

#[cfg(test)]
mod tests {
    use super::{DemuxerOptions, demuxer::Demuxer, symphonia_demuxer::SymphoniaDemuxer};

    #[test]
    fn compatibility_re_export_keeps_old_crate_path_available() {
        let options = DemuxerOptions::default();

        assert_eq!(
            options.max_consecutive_corrupted_packets(),
            DemuxerOptions::default().max_consecutive_corrupted_packets()
        );

        let _constructor = SymphoniaDemuxer::from_file;
        let _trait_name = std::any::type_name::<Box<dyn Demuxer + Send>>();
    }
}
