//! Чистая и ограниченная модель static MPEG-DASH MPD.
//!
//! Crate принимает только уже полученные XML bytes. Сеть, URL-разрешение,
//! demux, выбор Representation и состояние плеера принадлежат следующим слоям.

// Ошибки отделены от модели, чтобы runtime мог исчерпывающе различать причины.
mod error;
// Модель хранит только проверенные значения поддерживаемого VOD-профиля.
mod model;
// Parser является единственным входом из недоверенного XML в модель.
mod parser;
// Шаблоны и timeline имеют отдельную checked арифметику.
mod template;

// Публичные ошибки не раскрывают XML или адреса ресурсов.
pub use error::{DashMpdError, DashMpdErrorKind};
// Публичная модель является контрактом будущего S34B runtime.
pub use model::{
    DASH_MPD_NAMESPACE, DashAdaptationSet, DashAddressing, DashBaseUrl, DashContainer,
    DashInitialization, DashMediaKind, DashMpd, DashPeriod, DashRepresentation, DashSegmentBase,
    DashSegmentList, DashSegmentListEntry, DashSegmentTemplate, DashTimelineEntry,
    DashUrlReference, IndexRange,
};
// Parser request требует explicit XML и schema budgets.
pub use parser::{DashMpdLimits, DashMpdParseRequest, parse_dash_mpd};
// Expansion API не смешан с XML traversal.
pub use template::{
    DashSegmentPoint, DashTemplateContext, DashTemplateError, DashTemplateString,
    DashTimelineExpansion, expand_timeline,
};
