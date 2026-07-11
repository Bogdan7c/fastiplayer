//! Синтаксический разбор struct/field attributes без генерации токенов.

use proc_macro2::{Ident, Span};
use quote::format_ident;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Attribute, Data, Expr, Field, Fields, LitStr, Path, Token, parenthesized};

pub(super) fn named_struct_fields(data: &Data) -> syn::Result<&Punctuated<Field, Token![,]>> {
    match data {
        Data::Struct(data_struct) => match &data_struct.fields {
            Fields::Named(named_fields) => Ok(&named_fields.named),
            Fields::Unnamed(fields) => Err(syn::Error::new_spanned(
                fields,
                "SettingsSchema supports only structs with named fields",
            )),
            Fields::Unit => Err(syn::Error::new_spanned(
                &data_struct.fields,
                "SettingsSchema requires at least one named field",
            )),
        },
        Data::Enum(data_enum) => Err(syn::Error::new_spanned(
            data_enum.enum_token,
            "SettingsSchema cannot be derived for enums",
        )),
        Data::Union(data_union) => Err(syn::Error::new_spanned(
            data_union.union_token,
            "SettingsSchema cannot be derived for unions",
        )),
    }
}

#[derive(Default)]
pub(super) struct StructSettings {
    pub(super) require_all_fields: bool,
}

pub(super) fn parse_struct_settings(attrs: &[Attribute]) -> syn::Result<StructSettings> {
    let mut settings = StructSettings::default();

    for attr in attrs.iter().filter(|attr| attr.path().is_ident("settings")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("require_all_fields") {
                settings.require_all_fields = true;
                Ok(())
            } else {
                Err(meta.error("unknown #[settings(...)] option"))
            }
        })?;
    }

    Ok(settings)
}

pub(super) fn accessor_ident(struct_name: &Ident, field_ident: &Ident) -> Ident {
    let field_fragment = pascal_case_fragment(&field_ident.to_string());
    format_ident!(
        "SettingsSchemaAccessor{}{}",
        struct_name,
        field_fragment,
        span = field_ident.span()
    )
}

pub(super) fn pascal_case_fragment(value: &str) -> String {
    let mut fragment = String::new();
    for part in value.split('_').filter(|part| !part.is_empty()) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            fragment.extend(first.to_uppercase());
            fragment.push_str(chars.as_str());
        }
    }
    if fragment.is_empty() {
        "Field".to_owned()
    } else {
        fragment
    }
}

pub(super) enum FieldConfig {
    Nested,
    Setting(Box<CompleteFieldMetadata>),
    Skipped,
}

#[derive(Default)]
pub(super) struct FieldMetadata {
    pub(super) id: Option<LitStr>,
    pub(super) path: Option<LitStr>,
    pub(super) section: Option<LitStr>,
    pub(super) group: Option<LitStr>,
    pub(super) surface: Option<LitStr>,
    pub(super) label_id: Option<LitStr>,
    pub(super) label_ru: Option<LitStr>,
    pub(super) description_id: Option<LitStr>,
    pub(super) description_ru: Option<LitStr>,
    pub(super) help_id: Option<LitStr>,
    pub(super) help_ru: Option<LitStr>,
    pub(super) editor: Option<EditorMetadata>,
    pub(super) apply: Option<LitStr>,
    pub(super) route: Option<LitStr>,
    pub(super) read_only: bool,
    pub(super) default_behavior: Option<DefaultMetadata>,
    pub(super) option_provider: Option<LitStr>,
    pub(super) options: Vec<SelectOptionMetadata>,
    pub(super) min: Option<Expr>,
    pub(super) max: Option<Expr>,
    pub(super) step: Option<Expr>,
    pub(super) unit: Option<LitStr>,
    pub(super) expected_len: Option<Expr>,
    pub(super) vector_labels: Vec<LitStr>,
    pub(super) text_format: Option<TextFormatMetadata>,
    pub(super) min_len: Option<Expr>,
    pub(super) max_len: Option<Expr>,
}

pub(super) struct CompleteFieldMetadata {
    pub(super) id: LitStr,
    pub(super) path: LitStr,
    pub(super) section: LitStr,
    pub(super) group: LitStr,
    pub(super) surface: LitStr,
    pub(super) label_id: LitStr,
    pub(super) label_ru: LitStr,
    pub(super) description_id: Option<LitStr>,
    pub(super) description_ru: Option<LitStr>,
    pub(super) help_id: Option<LitStr>,
    pub(super) help_ru: Option<LitStr>,
    pub(super) editor: EditorMetadata,
    pub(super) apply: LitStr,
    pub(super) route: Option<LitStr>,
    pub(super) read_only: bool,
    pub(super) default_behavior: DefaultMetadata,
    pub(super) option_provider: Option<LitStr>,
    pub(super) options: Vec<SelectOptionMetadata>,
    pub(super) min: Option<Expr>,
    pub(super) max: Option<Expr>,
    pub(super) step: Option<Expr>,
    pub(super) unit: Option<LitStr>,
    pub(super) expected_len: Option<Expr>,
    pub(super) vector_labels: Vec<LitStr>,
    pub(super) text_format: TextFormatMetadata,
    pub(super) min_len: Option<Expr>,
    pub(super) max_len: Option<Expr>,
}

#[derive(Clone, Copy)]
pub(super) enum EditorMetadata {
    Toggle,
    Integer,
    Float,
    Select,
    SelectList,
    Text,
    Vector,
    ReadOnly,
}

#[derive(Clone, Copy)]
pub(super) enum DefaultMetadata {
    FromDefaultDocument,
    NoReset,
}

#[derive(Clone, Copy)]
pub(super) enum TextFormatMetadata {
    SingleLine,
    Multiline,
}

#[derive(Clone)]
pub(super) struct SelectOptionMetadata {
    pub(super) id: LitStr,
    pub(super) label_id: Option<LitStr>,
    pub(super) label_ru: Option<LitStr>,
    pub(super) value_path: Option<Path>,
}

pub(super) fn parse_field_config(
    field: &Field,
    require_all_fields: bool,
) -> syn::Result<FieldConfig> {
    let mut metadata = FieldMetadata::default();
    let mut saw_setting_attr = false;
    let mut nested = false;

    for attr in field
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("setting"))
    {
        saw_setting_attr = true;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("nested") {
                nested = true;
                return Ok(());
            }
            if meta.path.is_ident("read_only") {
                metadata.read_only = true;
                return Ok(());
            }
            if meta.path.is_ident("default") && !meta.input.peek(Token![=]) {
                metadata.default_behavior = Some(DefaultMetadata::FromDefaultDocument);
                return Ok(());
            }
            if meta.path.is_ident("options") {
                metadata.options = parse_options_list(meta.input)?;
                return Ok(());
            }
            if meta.path.is_ident("vector_labels") {
                metadata.vector_labels = parse_lit_str_list(meta.input)?;
                return Ok(());
            }

            let value = meta.value()?;
            if meta.path.is_ident("id") {
                metadata.id = Some(value.parse()?);
            } else if meta.path.is_ident("path") {
                metadata.path = Some(value.parse()?);
            } else if meta.path.is_ident("section") {
                metadata.section = Some(value.parse()?);
            } else if meta.path.is_ident("group") {
                metadata.group = Some(value.parse()?);
            } else if meta.path.is_ident("surface") || meta.path.is_ident("preferred_surface") {
                metadata.surface = Some(value.parse()?);
            } else if meta.path.is_ident("label_id") {
                metadata.label_id = Some(value.parse()?);
            } else if meta.path.is_ident("label_ru") || meta.path.is_ident("label") {
                metadata.label_ru = Some(value.parse()?);
            } else if meta.path.is_ident("description_id") {
                metadata.description_id = Some(value.parse()?);
            } else if meta.path.is_ident("description_ru") || meta.path.is_ident("description") {
                metadata.description_ru = Some(value.parse()?);
            } else if meta.path.is_ident("help_id") {
                metadata.help_id = Some(value.parse()?);
            } else if meta.path.is_ident("help_ru") || meta.path.is_ident("help") {
                metadata.help_ru = Some(value.parse()?);
            } else if meta.path.is_ident("editor") {
                metadata.editor = Some(parse_editor(value.parse()?)?);
            } else if meta.path.is_ident("apply") {
                metadata.apply = Some(value.parse()?);
            } else if meta.path.is_ident("route") {
                metadata.route = Some(value.parse()?);
            } else if meta.path.is_ident("option_provider") {
                metadata.option_provider = Some(value.parse()?);
            } else if meta.path.is_ident("default") {
                metadata.default_behavior = Some(parse_default_behavior(value.parse()?)?);
            } else if meta.path.is_ident("min") {
                metadata.min = Some(value.parse()?);
            } else if meta.path.is_ident("max") {
                metadata.max = Some(value.parse()?);
            } else if meta.path.is_ident("step") {
                metadata.step = Some(value.parse()?);
            } else if meta.path.is_ident("unit") {
                metadata.unit = Some(value.parse()?);
            } else if meta.path.is_ident("len") || meta.path.is_ident("expected_len") {
                metadata.expected_len = Some(value.parse()?);
            } else if meta.path.is_ident("format") {
                metadata.text_format = Some(parse_text_format(value.parse()?)?);
            } else if meta.path.is_ident("min_len") {
                metadata.min_len = Some(value.parse()?);
            } else if meta.path.is_ident("max_len") {
                metadata.max_len = Some(value.parse()?);
            } else {
                return Err(meta.error("unknown #[setting(...)] option"));
            }
            Ok(())
        })?;
    }

    if nested {
        return Ok(FieldConfig::Nested);
    }

    if !saw_setting_attr {
        if require_all_fields {
            return Err(syn::Error::new_spanned(
                field,
                "missing #[setting(...)] metadata; add metadata or mark the field #[setting(nested)]",
            ));
        }
        return Ok(FieldConfig::Skipped);
    }

    Ok(FieldConfig::Setting(Box::new(complete_metadata(
        field, metadata,
    )?)))
}

pub(super) fn complete_metadata(
    field: &Field,
    metadata: FieldMetadata,
) -> syn::Result<CompleteFieldMetadata> {
    Ok(CompleteFieldMetadata {
        id: required_metadata(field, metadata.id, "id")?,
        path: required_metadata(field, metadata.path, "path")?,
        section: required_metadata(field, metadata.section, "section")?,
        group: required_metadata(field, metadata.group, "group")?,
        surface: required_metadata(field, metadata.surface, "surface")?,
        label_id: required_metadata(field, metadata.label_id, "label_id")?,
        label_ru: required_metadata(field, metadata.label_ru, "label_ru")?,
        description_id: metadata.description_id,
        description_ru: metadata.description_ru,
        help_id: metadata.help_id,
        help_ru: metadata.help_ru,
        editor: required_metadata(field, metadata.editor, "editor")?,
        apply: required_metadata(field, metadata.apply, "apply")?,
        route: metadata.route,
        read_only: metadata.read_only,
        default_behavior: metadata
            .default_behavior
            .unwrap_or(DefaultMetadata::FromDefaultDocument),
        option_provider: metadata.option_provider,
        options: metadata.options,
        min: metadata.min,
        max: metadata.max,
        step: metadata.step,
        unit: metadata.unit,
        expected_len: metadata.expected_len,
        vector_labels: metadata.vector_labels,
        text_format: metadata
            .text_format
            .unwrap_or(TextFormatMetadata::SingleLine),
        min_len: metadata.min_len,
        max_len: metadata.max_len,
    })
}

pub(super) fn required_metadata<T>(field: &Field, value: Option<T>, name: &str) -> syn::Result<T> {
    value.ok_or_else(|| {
        syn::Error::new_spanned(
            field,
            format!("missing required #[setting(...)] metadata `{name}`"),
        )
    })
}

pub(super) fn parse_editor(value: LitStr) -> syn::Result<EditorMetadata> {
    let editor = match value.value().as_str() {
        "toggle" | "bool" | "boolean" => EditorMetadata::Toggle,
        "integer" | "int" => EditorMetadata::Integer,
        "float" | "number" => EditorMetadata::Float,
        "select" => EditorMetadata::Select,
        "select_list" | "ordered_select" | "select_sequence" => EditorMetadata::SelectList,
        "text" => EditorMetadata::Text,
        "vector" | "numeric_vector" => EditorMetadata::Vector,
        "read_only" | "readonly" => EditorMetadata::ReadOnly,
        _ => {
            return Err(syn::Error::new_spanned(
                value,
                "unknown settings editor; expected toggle, integer, float, select, select_list, text, vector or read_only",
            ));
        }
    };
    Ok(editor)
}

pub(super) fn parse_default_behavior(value: LitStr) -> syn::Result<DefaultMetadata> {
    let behavior = match value.value().as_str() {
        "document" | "from_document" | "from_default_document" => {
            DefaultMetadata::FromDefaultDocument
        }
        "none" | "no_reset" => DefaultMetadata::NoReset,
        _ => {
            return Err(syn::Error::new_spanned(
                value,
                "unknown default behavior; expected document or no_reset",
            ));
        }
    };
    Ok(behavior)
}

pub(super) fn parse_text_format(value: LitStr) -> syn::Result<TextFormatMetadata> {
    let format = match value.value().as_str() {
        "single_line" | "singleline" | "line" => TextFormatMetadata::SingleLine,
        "multiline" | "multi_line" => TextFormatMetadata::Multiline,
        _ => {
            return Err(syn::Error::new_spanned(
                value,
                "unknown text format; expected single_line or multiline",
            ));
        }
    };
    Ok(format)
}

pub(super) fn parse_lit_str_list(input: ParseStream<'_>) -> syn::Result<Vec<LitStr>> {
    let content;
    parenthesized!(content in input);
    let values = Punctuated::<LitStr, Token![,]>::parse_terminated(&content)?;
    Ok(values.into_iter().collect())
}

pub(super) fn parse_options_list(input: ParseStream<'_>) -> syn::Result<Vec<SelectOptionMetadata>> {
    let content;
    parenthesized!(content in input);

    if content.peek(LitStr) {
        let option_ids = Punctuated::<LitStr, Token![,]>::parse_terminated(&content)?;
        return Ok(option_ids
            .into_iter()
            .map(|id| SelectOptionMetadata {
                id,
                label_id: None,
                label_ru: None,
                value_path: None,
            })
            .collect());
    }

    let options = Punctuated::<OptionItem, Token![,]>::parse_terminated(&content)?;
    Ok(options.into_iter().map(|item| item.metadata).collect())
}

pub(super) struct OptionItem {
    pub(super) metadata: SelectOptionMetadata,
}

impl Parse for OptionItem {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let option_ident: Ident = input.parse()?;
        if option_ident != "option" {
            return Err(syn::Error::new_spanned(
                option_ident,
                "expected option(id = ..., value = ...) inside options(...)",
            ));
        }

        let content;
        parenthesized!(content in input);
        let mut id = None;
        let mut label_id = None;
        let mut label_ru = None;
        let mut value_path = None;

        while !content.is_empty() {
            let key: Ident = content.parse()?;
            content.parse::<Token![=]>()?;

            if key == "id" {
                id = Some(content.parse()?);
            } else if key == "label_id" {
                label_id = Some(content.parse()?);
            } else if key == "label_ru" || key == "label" {
                label_ru = Some(content.parse()?);
            } else if key == "value" {
                value_path = Some(content.parse()?);
            } else {
                return Err(syn::Error::new_spanned(
                    key,
                    "unknown option(...) key; expected id, label_id, label_ru or value",
                ));
            }

            if content.peek(Token![,]) {
                content.parse::<Token![,]>()?;
            }
        }

        Ok(Self {
            metadata: SelectOptionMetadata {
                id: id
                    .ok_or_else(|| syn::Error::new(Span::call_site(), "option(...) requires id"))?,
                label_id,
                label_ru,
                value_path,
            },
        })
    }
}
