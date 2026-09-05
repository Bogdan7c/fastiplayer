//! Caller-visible XSPF budgets без scattered parser literals.

use bounded_xml_reader::{XmlBudgets, XmlBudgetsBuilder};

/// Default XSPF document budget ограничивает один уже полученный byte slice.
pub const DEFAULT_MAX_XSPF_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
/// XSPF schema неглубокая; запас оставляет bounded unknown extension subtree.
pub const DEFAULT_MAX_XSPF_DEPTH: usize = 32;
/// Token budget ограничивает документы с огромным числом пустых constructs.
pub const DEFAULT_MAX_XSPF_TOKENS: usize = 250_000;
/// Один XSPF/Fastiplayer element не нуждается в десятках attributes.
pub const DEFAULT_MAX_XSPF_ATTRIBUTES_PER_ELEMENT: usize = 16;
/// Total attribute count закрывает distributed attribute flood.
pub const DEFAULT_MAX_XSPF_ATTRIBUTE_COUNT: usize = 100_000;
/// Materialized attribute bytes имеют отдельный allocation budget.
pub const DEFAULT_MAX_XSPF_ATTRIBUTE_BYTES: usize = 512 * 1024;
/// Per-element namespace cap допускает обычные extensions без namespace bomb.
pub const DEFAULT_MAX_XSPF_NAMESPACE_DECLARATIONS_PER_ELEMENT: usize = 8;
/// Total namespace count ограничивает повторные declarations по документу.
pub const DEFAULT_MAX_XSPF_NAMESPACE_DECLARATIONS: usize = 128;
/// Namespace prefix/URI bytes учитываются отдельно от обычных attributes.
pub const DEFAULT_MAX_XSPF_NAMESPACE_BYTES: usize = 32 * 1024;
/// Decoded text budget ограничивает metadata и URI materialization.
pub const DEFAULT_MAX_XSPF_TEXT_BYTES: usize = 2 * 1024 * 1024;
/// Track count совпадает с canonical retained-capacity ceiling.
pub const DEFAULT_MAX_XSPF_TRACKS: usize = playlist_core::MAX_PLAYLIST_ITEMS;
/// Несколько fallback locations полезны, но unbounded candidate list не нужна.
pub const DEFAULT_MAX_XSPF_LOCATIONS_PER_TRACK: usize = 32;
/// Group count не может практически превысить число flattened tracks.
pub const DEFAULT_MAX_XSPF_GROUPS: usize = playlist_core::MAX_PLAYLIST_ITEMS;

/// Полный immutable budget profile одного XSPF parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XspfParserLimits {
    /// Hardened reader применяет эти budgets до schema interpretation.
    xml_budgets: XmlBudgets,
    /// Parser не materialize-ит больше указанного числа tracks.
    maximum_tracks: usize,
    /// Каждый track имеет отдельный ordered location-candidate cap.
    maximum_locations_per_track: usize,
    /// Fastiplayer extension имеет отдельный group-record cap.
    maximum_groups: usize,
}

impl XspfParserLimits {
    /// Создаёт profile из явно собранных XML budgets.
    pub const fn new(xml_budgets: XmlBudgets) -> Self {
        Self {
            xml_budgets,
            maximum_tracks: DEFAULT_MAX_XSPF_TRACKS,
            maximum_locations_per_track: DEFAULT_MAX_XSPF_LOCATIONS_PER_TRACK,
            maximum_groups: DEFAULT_MAX_XSPF_GROUPS,
        }
    }

    /// Задаёт maximum flattened tracks.
    pub const fn with_maximum_tracks(mut self, maximum: usize) -> Self {
        self.maximum_tracks = maximum;
        self
    }

    /// Задаёт maximum ordered locations одного track-а.
    pub const fn with_maximum_locations_per_track(mut self, maximum: usize) -> Self {
        self.maximum_locations_per_track = maximum;
        self
    }

    /// Задаёт maximum Fastiplayer group records.
    pub const fn with_maximum_groups(mut self, maximum: usize) -> Self {
        self.maximum_groups = maximum;
        self
    }

    /// Возвращает complete hardened XML budgets.
    pub const fn xml_budgets(self) -> XmlBudgets {
        self.xml_budgets
    }

    /// Возвращает flattened track cap.
    pub const fn maximum_tracks(self) -> usize {
        self.maximum_tracks
    }

    /// Возвращает per-track location cap.
    pub const fn maximum_locations_per_track(self) -> usize {
        self.maximum_locations_per_track
    }

    /// Возвращает group-record cap.
    pub const fn maximum_groups(self) -> usize {
        self.maximum_groups
    }
}

impl Default for XspfParserLimits {
    fn default() -> Self {
        // Каждый XML budget называется на месте сборки и не скрыт в reader-е.
        let xml_budgets = XmlBudgetsBuilder::new()
            .maximum_document_bytes(DEFAULT_MAX_XSPF_DOCUMENT_BYTES)
            .maximum_depth(DEFAULT_MAX_XSPF_DEPTH)
            .maximum_tokens(DEFAULT_MAX_XSPF_TOKENS)
            .maximum_attributes_per_element(DEFAULT_MAX_XSPF_ATTRIBUTES_PER_ELEMENT)
            .maximum_attribute_count(DEFAULT_MAX_XSPF_ATTRIBUTE_COUNT)
            .maximum_attribute_bytes(DEFAULT_MAX_XSPF_ATTRIBUTE_BYTES)
            .maximum_namespace_declarations_per_element(
                DEFAULT_MAX_XSPF_NAMESPACE_DECLARATIONS_PER_ELEMENT,
            )
            .maximum_namespace_declaration_count(DEFAULT_MAX_XSPF_NAMESPACE_DECLARATIONS)
            .maximum_namespace_bytes(DEFAULT_MAX_XSPF_NAMESPACE_BYTES)
            .maximum_text_bytes(DEFAULT_MAX_XSPF_TEXT_BYTES)
            .build()
            .expect("default XSPF XML profile names every mandatory budget");
        // Format-specific caps остаются видимыми рядом с XML profile.
        Self::new(xml_budgets)
    }
}
