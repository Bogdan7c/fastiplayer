//! App-owned multi-factory demux composition для web candidate planning/open.
//!
//! Модуль агрегирует exact per-container registrations; concrete parsers не
//! узнают друг о друге, а planner не получает factory-wide capability union.

use anyhow::{Context, Result, anyhow};
use demux_api::{DemuxFactory, DemuxFactoryDescriptor, DemuxRegistry};
use flv_demux::{FlvDemuxFactory, FlvDemuxOptions};
use mpeg_ts_demux::{MpegTsDemuxFactory, MpegTsDemuxOptions};
use symphonia_demux::{DemuxerOptions, SymphoniaDemuxFactory};
use web_media_core::ContainerFamily;
use web_media_playback_plan::{DemuxCapabilityRegistration, DemuxCapabilitySnapshot};

/// Готовая registry и точно соответствующий ей immutable planner snapshot.
pub(crate) struct WebDemuxComposition {
    /// Concrete factories, которыми runtime фактически может открыть input.
    pub(crate) registry: DemuxRegistry,
    /// Per-container capability rows из тех же descriptor-ов.
    pub(crate) capabilities: DemuxCapabilitySnapshot,
}

impl WebDemuxComposition {
    /// Регистрирует existing Symphonia и S30 FLV/F4F owners в одном месте.
    pub(crate) fn new(symphonia_options: DemuxerOptions) -> Result<Self> {
        let factories: Vec<Box<dyn DemuxFactory>> = vec![
            Box::new(
                SymphoniaDemuxFactory::new(symphonia_options)
                    .context("Не удалось создать Symphonia demux factory")?,
            ),
            Box::new(
                FlvDemuxFactory::new(FlvDemuxOptions::default())
                    .context("Не удалось создать FLV/F4F demux factory")?,
            ),
        ];
        let capabilities =
            capabilities_for_descriptors(factories.iter().map(|factory| factory.descriptor()))?;
        let mut registry = DemuxRegistry::new();
        for factory in factories {
            registry
                .register(factory)
                .context("Не удалось зарегистрировать web demux factory")?;
        }
        Ok(Self {
            registry,
            capabilities,
        })
    }

    /// Регистрирует только concrete TS/fMP4 owners для HLS ordered-segment path-а.
    pub(crate) fn new_hls(
        symphonia_options: DemuxerOptions,
        mpeg_ts_options: MpegTsDemuxOptions,
    ) -> Result<Self> {
        let factories: Vec<Box<dyn DemuxFactory>> = vec![
            Box::new(
                SymphoniaDemuxFactory::new(symphonia_options)
                    .context("Не удалось создать HLS Symphonia demux factory")?,
            ),
            Box::new(
                MpegTsDemuxFactory::new(mpeg_ts_options)
                    .context("Не удалось создать HLS MPEG-TS demux factory")?,
            ),
        ];
        let capabilities =
            capabilities_for_descriptors(factories.iter().map(|factory| factory.descriptor()))?;
        let mut registry = DemuxRegistry::new();
        for factory in factories {
            registry
                .register(factory)
                .context("Не удалось зарегистрировать HLS demux factory")?;
        }
        Ok(Self {
            registry,
            capabilities,
        })
    }
}

/// Строит planner snapshot без потери exact per-registration input sets.
pub(crate) fn capabilities_for_descriptors<'a>(
    descriptors: impl IntoIterator<Item = &'a DemuxFactoryDescriptor>,
) -> Result<DemuxCapabilitySnapshot> {
    let mut registrations = Vec::new();
    for descriptor in descriptors {
        for registration in &descriptor.containers {
            let families = container_families_for_demux_id(registration.container.as_str())
                .ok_or_else(|| anyhow!("Неизвестный container ID в demux descriptor"))?;
            for family in families {
                registrations.push(DemuxCapabilityRegistration::new(
                    *family,
                    registration.input_capabilities(),
                )?);
            }
        }
    }
    Ok(DemuxCapabilitySnapshot::new(registrations))
}

/// Явно связывает concrete registry identity с neutral planning family.
fn container_families_for_demux_id(id: &str) -> Option<&'static [ContainerFamily]> {
    match id {
        "iso-bmff" => Some(&[ContainerFamily::IsoBmff, ContainerFamily::FragmentedIsoBmff]),
        "matroska" => Some(&[ContainerFamily::Matroska]),
        "webm" => Some(&[ContainerFamily::WebM]),
        "ogg" => Some(&[ContainerFamily::Ogg]),
        "caf" => Some(&[ContainerFamily::Caf]),
        "wave" => Some(&[ContainerFamily::Wav]),
        "aiff" => Some(&[ContainerFamily::Aiff]),
        "flac" => Some(&[ContainerFamily::Flac]),
        "mpeg-audio" => Some(&[ContainerFamily::MpegAudio]),
        "flv" => Some(&[ContainerFamily::Flv]),
        "f4f" => Some(&[ContainerFamily::F4f]),
        "mpeg-ts" => Some(&[ContainerFamily::MpegTs]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use demux_api::{DemuxInputCapabilities, DemuxInputCapability};
    use flv_demux::{FlvDemuxFactory, FlvDemuxOptions};
    use web_media_core::ContainerFamily;

    use super::*;

    #[test]
    fn flv_and_f4f_capabilities_keep_exact_input_shapes() {
        let factory = FlvDemuxFactory::new(FlvDemuxOptions::default()).expect("FLV factory");
        let capabilities =
            capabilities_for_descriptors([factory.descriptor()]).expect("capability snapshot");
        assert_eq!(
            capabilities.input_capabilities_for(ContainerFamily::Flv),
            DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
                .with(DemuxInputCapability::StreamingBytes)
        );
        assert_eq!(
            capabilities.input_capabilities_for(ContainerFamily::F4f),
            DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments)
        );
        assert_eq!(
            capabilities.input_capabilities_for(ContainerFamily::IsoBmff),
            DemuxInputCapabilities::default()
        );
    }

    /// S30 web composition не подтягивает accidental MPEG-TS registration из S29.
    #[test]
    fn production_web_composition_excludes_mpeg_ts() {
        let composition =
            WebDemuxComposition::new(DemuxerOptions::default()).expect("web demux composition");
        assert_eq!(
            composition
                .capabilities
                .input_capabilities_for(ContainerFamily::MpegTs),
            DemuxInputCapabilities::default()
        );
    }

    #[test]
    fn hls_composition_adds_ordered_ts_without_changing_progressive_composition() {
        let composition =
            WebDemuxComposition::new_hls(DemuxerOptions::default(), MpegTsDemuxOptions::default())
                .expect("HLS composition");
        assert!(
            composition
                .capabilities
                .input_capabilities_for(ContainerFamily::MpegTs)
                .contains(DemuxInputCapability::OrderedSegments)
        );
    }
}
