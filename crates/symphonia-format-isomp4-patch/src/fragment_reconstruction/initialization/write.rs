//! Deterministic serializer проверенного initialization plan-а.

use super::error::FragmentInitializationError;
use super::plan::{FragmentInitializationPlan, InitializationBoxSizes, PlannedInitializationCodec};

const IDENTITY_MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];
const UNDEFINED_LANGUAGE: u16 = 0x55c4;

/// Выделяет единственный planned buffer и записывает доказанное дерево boxes.
pub(super) fn write_initialization_segment(
    plan: &FragmentInitializationPlan<'_>,
) -> Result<Vec<u8>, FragmentInitializationError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(plan.total_size())
        .map_err(|_| FragmentInitializationError::AllocationFailed)?;
    let mut writer = PlannedWriter::new(bytes, plan.total_size());

    write_file_type(&mut writer, plan.sizes())?;
    write_movie(&mut writer, plan)?;

    writer.finish()
}

fn write_file_type(
    writer: &mut PlannedWriter,
    sizes: InitializationBoxSizes,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(sizes.file_type, *b"ftyp")?;
    writer.bytes(b"iso6")?;
    writer.u32(1)?;
    writer.bytes(b"isom")?;
    writer.bytes(b"iso6")?;
    writer.bytes(b"mp41")
}

fn write_movie(
    writer: &mut PlannedWriter,
    plan: &FragmentInitializationPlan<'_>,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(plan.sizes().movie, *b"moov")?;
    write_movie_header(writer, plan)?;
    write_track(writer, plan)?;
    write_movie_extends(writer, plan)
}

fn write_movie_header(
    writer: &mut PlannedWriter,
    plan: &FragmentInitializationPlan<'_>,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(108, *b"mvhd")?;
    writer.full_box_header(0, 0)?;
    writer.zeros(8)?;
    writer.u32(plan.timescale().get())?;
    writer.u32(0)?;
    writer.u32(0x0001_0000)?;
    writer.u16(0x0100)?;
    writer.zeros(10)?;
    writer.matrix()?;
    writer.zeros(24)?;
    writer.u32(plan.next_track_id())
}

fn write_track(
    writer: &mut PlannedWriter,
    plan: &FragmentInitializationPlan<'_>,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(plan.sizes().track, *b"trak")?;
    write_track_header(writer, plan)?;
    write_media(writer, plan)
}

fn write_track_header(
    writer: &mut PlannedWriter,
    plan: &FragmentInitializationPlan<'_>,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(92, *b"tkhd")?;
    writer.full_box_header(0, 0x0000_0007)?;
    writer.zeros(8)?;
    writer.u32(plan.track_id().get())?;
    writer.u32(0)?;
    writer.u32(0)?;
    writer.zeros(8)?;
    let volume = match plan.codec() {
        PlannedInitializationCodec::H264 { .. } => 0,
        PlannedInitializationCodec::Aac { .. } => 0x0100,
    };
    writer.u16(0)?;
    writer.u16(0)?;
    writer.u16(volume)?;
    writer.u16(0)?;
    writer.matrix()?;
    match plan.codec() {
        PlannedInitializationCodec::H264 { configuration, .. } => {
            writer.u32(u32::from(configuration.dimensions().width().get()) << 16)?;
            writer.u32(u32::from(configuration.dimensions().height().get()) << 16)
        }
        PlannedInitializationCodec::Aac { .. } => {
            writer.u32(0)?;
            writer.u32(0)
        }
    }
}

fn write_media(
    writer: &mut PlannedWriter,
    plan: &FragmentInitializationPlan<'_>,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(plan.sizes().media, *b"mdia")?;
    write_media_header(writer, plan)?;
    write_handler(writer, plan.codec())?;
    write_media_information(writer, plan)
}

fn write_media_header(
    writer: &mut PlannedWriter,
    plan: &FragmentInitializationPlan<'_>,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(32, *b"mdhd")?;
    writer.full_box_header(0, 0)?;
    writer.zeros(8)?;
    writer.u32(plan.timescale().get())?;
    writer.u32(0)?;
    writer.u16(UNDEFINED_LANGUAGE)?;
    writer.u16(0)
}

fn write_handler(
    writer: &mut PlannedWriter,
    codec: PlannedInitializationCodec<'_>,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(32, *b"hdlr")?;
    writer.full_box_header(0, 0)?;
    writer.u32(0)?;
    match codec {
        PlannedInitializationCodec::H264 { .. } => writer.bytes(b"vide")?,
        PlannedInitializationCodec::Aac { .. } => writer.bytes(b"soun")?,
    }
    writer.zeros(12)
}

fn write_media_information(
    writer: &mut PlannedWriter,
    plan: &FragmentInitializationPlan<'_>,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(plan.sizes().media_information, *b"minf")?;
    match plan.codec() {
        PlannedInitializationCodec::H264 { .. } => write_video_media_header(writer)?,
        PlannedInitializationCodec::Aac { .. } => write_sound_media_header(writer)?,
    }
    write_data_information(writer)?;
    write_sample_table(writer, plan)
}

fn write_video_media_header(writer: &mut PlannedWriter) -> Result<(), FragmentInitializationError> {
    writer.box_header(20, *b"vmhd")?;
    writer.full_box_header(0, 1)?;
    writer.zeros(8)
}

fn write_sound_media_header(writer: &mut PlannedWriter) -> Result<(), FragmentInitializationError> {
    writer.box_header(16, *b"smhd")?;
    writer.full_box_header(0, 0)?;
    writer.zeros(4)
}

fn write_data_information(writer: &mut PlannedWriter) -> Result<(), FragmentInitializationError> {
    writer.box_header(36, *b"dinf")?;
    writer.box_header(28, *b"dref")?;
    writer.full_box_header(0, 0)?;
    writer.u32(1)?;
    writer.box_header(12, *b"url ")?;
    writer.full_box_header(0, 1)
}

fn write_sample_table(
    writer: &mut PlannedWriter,
    plan: &FragmentInitializationPlan<'_>,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(plan.sizes().sample_table, *b"stbl")?;
    write_sample_description(writer, plan)?;
    write_empty_table(writer, 16, *b"stts")?;
    write_empty_table(writer, 16, *b"stsc")?;
    write_empty_sample_size(writer)?;
    write_empty_table(writer, 16, *b"stco")
}

fn write_sample_description(
    writer: &mut PlannedWriter,
    plan: &FragmentInitializationPlan<'_>,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(plan.sizes().sample_description, *b"stsd")?;
    writer.full_box_header(0, 0)?;
    writer.u32(1)?;
    match plan.codec() {
        PlannedInitializationCodec::H264 {
            configuration,
            avc_configuration_size,
            sample_entry_size,
        } => write_avc_sample_entry(
            writer,
            configuration,
            avc_configuration_size,
            sample_entry_size,
        ),
        PlannedInitializationCodec::Aac {
            configuration,
            elementary_stream_descriptor_size,
            sample_entry_size,
        } => write_aac_sample_entry(
            writer,
            configuration,
            elementary_stream_descriptor_size,
            sample_entry_size,
        ),
    }
}

fn write_avc_sample_entry(
    writer: &mut PlannedWriter,
    configuration: super::model::FragmentH264Configuration<'_>,
    avc_configuration_size: u32,
    sample_entry_size: u32,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(sample_entry_size, *b"avc1")?;
    writer.zeros(6)?;
    writer.u16(1)?;
    writer.zeros(16)?;
    writer.u16(configuration.dimensions().width().get())?;
    writer.u16(configuration.dimensions().height().get())?;
    writer.u32(0x0048_0000)?;
    writer.u32(0x0048_0000)?;
    writer.u32(0)?;
    writer.u16(1)?;
    writer.zeros(32)?;
    writer.u16(0x0018)?;
    writer.u16(u16::MAX)?;
    write_avc_configuration(writer, configuration, avc_configuration_size)
}

fn write_avc_configuration(
    writer: &mut PlannedWriter,
    configuration: super::model::FragmentH264Configuration<'_>,
    size: u32,
) -> Result<(), FragmentInitializationError> {
    let sequence_parameter_set = configuration.sequence_parameter_set().as_bytes();
    let picture_parameter_set = configuration.picture_parameter_set().as_bytes();
    writer.box_header(size, *b"avcC")?;
    writer.u8(1)?;
    writer.u8(sequence_parameter_set[1])?;
    writer.u8(sequence_parameter_set[2])?;
    writer.u8(sequence_parameter_set[3])?;
    writer.u8(0xff)?;
    writer.u8(0xe1)?;
    writer.u16(
        u16::try_from(sequence_parameter_set.len())
            .map_err(|_| FragmentInitializationError::SerializationInvariantViolated)?,
    )?;
    writer.bytes(sequence_parameter_set)?;
    writer.u8(1)?;
    writer.u16(
        u16::try_from(picture_parameter_set.len())
            .map_err(|_| FragmentInitializationError::SerializationInvariantViolated)?,
    )?;
    writer.bytes(picture_parameter_set)
}

fn write_aac_sample_entry(
    writer: &mut PlannedWriter,
    configuration: super::model::FragmentAacLcConfiguration<'_>,
    elementary_stream_descriptor_size: u32,
    sample_entry_size: u32,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(sample_entry_size, *b"mp4a")?;
    writer.zeros(6)?;
    writer.u16(1)?;
    writer.zeros(8)?;
    writer.u16(configuration.channel_count().get())?;
    writer.u16(16)?;
    writer.zeros(4)?;
    writer.u32(configuration.sample_rate().get() << 16)?;
    write_elementary_stream_descriptor(writer, configuration, elementary_stream_descriptor_size)
}

fn write_elementary_stream_descriptor(
    writer: &mut PlannedWriter,
    configuration: super::model::FragmentAacLcConfiguration<'_>,
    size: u32,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(size, *b"esds")?;
    writer.full_box_header(0, 0)?;
    writer.bytes(&[0x03, 0x19, 0x00, 0x01, 0x00])?;
    writer.bytes(&[0x04, 0x11, 0x40, 0x15])?;
    writer.zeros(11)?;
    writer.bytes(&[0x05, 0x02])?;
    writer.bytes(configuration.audio_specific_config().as_bytes())?;
    writer.bytes(&[0x06, 0x01, 0x02])
}

fn write_empty_table(
    writer: &mut PlannedWriter,
    size: u32,
    box_type: [u8; 4],
) -> Result<(), FragmentInitializationError> {
    writer.box_header(size, box_type)?;
    writer.full_box_header(0, 0)?;
    writer.u32(0)
}

fn write_empty_sample_size(writer: &mut PlannedWriter) -> Result<(), FragmentInitializationError> {
    writer.box_header(20, *b"stsz")?;
    writer.full_box_header(0, 0)?;
    writer.u32(0)?;
    writer.u32(0)
}

fn write_movie_extends(
    writer: &mut PlannedWriter,
    plan: &FragmentInitializationPlan<'_>,
) -> Result<(), FragmentInitializationError> {
    writer.box_header(plan.sizes().movie_extends, *b"mvex")?;
    writer.box_header(32, *b"trex")?;
    writer.full_box_header(0, 0)?;
    writer.u32(plan.track_id().get())?;
    writer.u32(1)?;
    writer.u32(0)?;
    writer.u32(0)?;
    writer.u32(0)
}

/// Writer не может выйти за planned length и тем самым вызвать вторую allocation.
struct PlannedWriter {
    bytes: Vec<u8>,
    planned_length: usize,
}

impl PlannedWriter {
    fn new(bytes: Vec<u8>, planned_length: usize) -> Self {
        Self {
            bytes,
            planned_length,
        }
    }

    fn finish(self) -> Result<Vec<u8>, FragmentInitializationError> {
        if self.bytes.len() != self.planned_length {
            return Err(FragmentInitializationError::SerializationInvariantViolated);
        }
        Ok(self.bytes)
    }

    fn remaining(&self) -> usize {
        self.planned_length.saturating_sub(self.bytes.len())
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), FragmentInitializationError> {
        if value.len() > self.remaining() {
            return Err(FragmentInitializationError::SerializationInvariantViolated);
        }
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn zeros(&mut self, count: usize) -> Result<(), FragmentInitializationError> {
        if count > self.remaining() {
            return Err(FragmentInitializationError::SerializationInvariantViolated);
        }
        self.bytes.resize(self.bytes.len() + count, 0);
        Ok(())
    }

    fn u8(&mut self, value: u8) -> Result<(), FragmentInitializationError> {
        self.bytes(&[value])
    }

    fn u16(&mut self, value: u16) -> Result<(), FragmentInitializationError> {
        self.bytes(&value.to_be_bytes())
    }

    fn u32(&mut self, value: u32) -> Result<(), FragmentInitializationError> {
        self.bytes(&value.to_be_bytes())
    }

    fn box_header(
        &mut self,
        size: u32,
        box_type: [u8; 4],
    ) -> Result<(), FragmentInitializationError> {
        self.u32(size)?;
        self.bytes(&box_type)
    }

    fn full_box_header(
        &mut self,
        version: u8,
        flags: u32,
    ) -> Result<(), FragmentInitializationError> {
        if flags > 0x00ff_ffff {
            return Err(FragmentInitializationError::SerializationInvariantViolated);
        }
        self.u8(version)?;
        self.bytes(&flags.to_be_bytes()[1..])
    }

    fn matrix(&mut self) -> Result<(), FragmentInitializationError> {
        for value in IDENTITY_MATRIX {
            self.u32(value)?;
        }
        Ok(())
    }
}
