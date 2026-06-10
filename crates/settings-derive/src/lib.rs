//! Derive macro for generated `settings-core` registries.
//!
//! Русский комментарий: этот crate не владеет настройками приложения. Он только
//! читает metadata рядом со schema struct и генерирует тонкие accessors поверх
//! публичных контрактов `settings-core`.

use proc_macro::TokenStream;
use proc_macro2::{Ident, Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Data, DeriveInput, Expr, Field, Fields, LitStr, Path, Token, Type, parenthesized,
    parse_macro_input,
};

#[proc_macro_derive(SettingsSchema, attributes(settings, setting))]
pub fn derive_settings_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    expand_settings_schema(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_settings_schema(input: DeriveInput) -> syn::Result<TokenStream2> {
    let struct_settings = parse_struct_settings(&input.attrs)?;
    let struct_name = input.ident.clone();
    let fields = named_struct_fields(&input.data)?;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    let impl_generics_tokens = quote! { #impl_generics };
    let type_generics_tokens = quote! { #type_generics };
    let where_clause_tokens = quote! { #where_clause };
    let mut accessor_impls = Vec::new();
    let mut registry_steps = Vec::new();

    for field in fields {
        let field_ident = field.ident.as_ref().ok_or_else(|| {
            syn::Error::new_spanned(field, "SettingsSchema supports only named fields")
        })?;
        let field_config = parse_field_config(field, struct_settings.require_all_fields)?;

        match field_config {
            FieldConfig::Nested => {
                registry_steps.push(expand_nested_field(field_ident, &field.ty));
            }
            FieldConfig::Setting(metadata) => {
                let metadata = *metadata;
                let accessor_name = accessor_ident(&struct_name, field_ident);
                let access_kind = infer_access_kind(field, &metadata)?;
                accessor_impls.push(expand_accessor_impl(AccessorExpansion {
                    struct_name: &struct_name,
                    impl_generics: &impl_generics_tokens,
                    type_generics: &type_generics_tokens,
                    where_clause: &where_clause_tokens,
                    accessor_name: &accessor_name,
                    field_ident,
                    field_type: &field.ty,
                    access_kind: &access_kind,
                    setting_id: metadata.id.value(),
                }));
                registry_steps.push(expand_setting_registration(
                    &accessor_name,
                    &metadata,
                    &access_kind,
                )?);
            }
            FieldConfig::Skipped => {}
        }
    }

    Ok(quote! {
        #(#accessor_impls)*

        impl #impl_generics ::settings_core::SettingsSchema for #struct_name #type_generics #where_clause {
            fn settings_registry() -> ::settings_core::SettingsResult<::settings_core::SettingsRegistry<Self>> {
                let mut registry = ::settings_core::SettingsRegistry::<Self>::empty();
                #(#registry_steps)*
                Ok(registry)
            }
        }
    })
}

fn named_struct_fields(data: &Data) -> syn::Result<&Punctuated<Field, Token![,]>> {
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
struct StructSettings {
    require_all_fields: bool,
}

fn parse_struct_settings(attrs: &[Attribute]) -> syn::Result<StructSettings> {
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

fn accessor_ident(struct_name: &Ident, field_ident: &Ident) -> Ident {
    let field_fragment = pascal_case_fragment(&field_ident.to_string());
    format_ident!(
        "SettingsSchemaAccessor{}{}",
        struct_name,
        field_fragment,
        span = field_ident.span()
    )
}

fn pascal_case_fragment(value: &str) -> String {
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

enum FieldConfig {
    Nested,
    Setting(Box<CompleteFieldMetadata>),
    Skipped,
}

#[derive(Default)]
struct FieldMetadata {
    id: Option<LitStr>,
    path: Option<LitStr>,
    section: Option<LitStr>,
    group: Option<LitStr>,
    surface: Option<LitStr>,
    label_id: Option<LitStr>,
    label_ru: Option<LitStr>,
    description_id: Option<LitStr>,
    description_ru: Option<LitStr>,
    help_id: Option<LitStr>,
    help_ru: Option<LitStr>,
    editor: Option<EditorMetadata>,
    apply: Option<LitStr>,
    route: Option<LitStr>,
    read_only: bool,
    default_behavior: Option<DefaultMetadata>,
    option_provider: Option<LitStr>,
    options: Vec<SelectOptionMetadata>,
    min: Option<Expr>,
    max: Option<Expr>,
    step: Option<Expr>,
    unit: Option<LitStr>,
    expected_len: Option<Expr>,
    vector_labels: Vec<LitStr>,
    text_format: Option<TextFormatMetadata>,
    min_len: Option<Expr>,
    max_len: Option<Expr>,
}

struct CompleteFieldMetadata {
    id: LitStr,
    path: LitStr,
    section: LitStr,
    group: LitStr,
    surface: LitStr,
    label_id: LitStr,
    label_ru: LitStr,
    description_id: Option<LitStr>,
    description_ru: Option<LitStr>,
    help_id: Option<LitStr>,
    help_ru: Option<LitStr>,
    editor: EditorMetadata,
    apply: LitStr,
    route: Option<LitStr>,
    read_only: bool,
    default_behavior: DefaultMetadata,
    option_provider: Option<LitStr>,
    options: Vec<SelectOptionMetadata>,
    min: Option<Expr>,
    max: Option<Expr>,
    step: Option<Expr>,
    unit: Option<LitStr>,
    expected_len: Option<Expr>,
    vector_labels: Vec<LitStr>,
    text_format: TextFormatMetadata,
    min_len: Option<Expr>,
    max_len: Option<Expr>,
}

#[derive(Clone, Copy)]
enum EditorMetadata {
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
enum DefaultMetadata {
    FromDefaultDocument,
    NoReset,
}

#[derive(Clone, Copy)]
enum TextFormatMetadata {
    SingleLine,
    Multiline,
}

#[derive(Clone)]
struct SelectOptionMetadata {
    id: LitStr,
    label_id: Option<LitStr>,
    label_ru: Option<LitStr>,
    value_path: Option<Path>,
}

fn parse_field_config(field: &Field, require_all_fields: bool) -> syn::Result<FieldConfig> {
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

fn complete_metadata(field: &Field, metadata: FieldMetadata) -> syn::Result<CompleteFieldMetadata> {
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

fn required_metadata<T>(field: &Field, value: Option<T>, name: &str) -> syn::Result<T> {
    value.ok_or_else(|| {
        syn::Error::new_spanned(
            field,
            format!("missing required #[setting(...)] metadata `{name}`"),
        )
    })
}

fn parse_editor(value: LitStr) -> syn::Result<EditorMetadata> {
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

fn parse_default_behavior(value: LitStr) -> syn::Result<DefaultMetadata> {
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

fn parse_text_format(value: LitStr) -> syn::Result<TextFormatMetadata> {
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

fn parse_lit_str_list(input: ParseStream<'_>) -> syn::Result<Vec<LitStr>> {
    let content;
    parenthesized!(content in input);
    let values = Punctuated::<LitStr, Token![,]>::parse_terminated(&content)?;
    Ok(values.into_iter().collect())
}

fn parse_options_list(input: ParseStream<'_>) -> syn::Result<Vec<SelectOptionMetadata>> {
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

struct OptionItem {
    metadata: SelectOptionMetadata,
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

enum AccessKind {
    Bool,
    Integer,
    Float,
    Text,
    SelectString,
    SelectEnum(Vec<SelectOptionMetadata>),
    SelectListString,
    SelectListEnum(Vec<SelectOptionMetadata>),
    NumericVector(Type),
}

fn infer_access_kind(field: &Field, metadata: &CompleteFieldMetadata) -> syn::Result<AccessKind> {
    match metadata.editor {
        EditorMetadata::Toggle => {
            require_type_kind(field, is_bool_type, "toggle editor requires a bool field")?;
            Ok(AccessKind::Bool)
        }
        EditorMetadata::Integer => {
            require_type_kind(
                field,
                is_integer_type,
                "integer editor requires an integer field",
            )?;
            require_numeric_range(field, metadata)?;
            Ok(AccessKind::Integer)
        }
        EditorMetadata::Float => {
            require_type_kind(
                field,
                is_float_type,
                "float editor requires an f32 or f64 field",
            )?;
            require_numeric_range(field, metadata)?;
            Ok(AccessKind::Float)
        }
        EditorMetadata::Text => {
            require_type_kind(field, is_string_type, "text editor requires a String field")?;
            Ok(AccessKind::Text)
        }
        EditorMetadata::Select => infer_select_access_kind(field, metadata),
        EditorMetadata::SelectList => infer_select_list_access_kind(field, metadata),
        EditorMetadata::Vector => {
            let element_type = vec_element_type(&field.ty).ok_or_else(|| {
                syn::Error::new_spanned(field, "vector editor requires Vec<f32> or Vec<f64>")
            })?;
            if !is_f32_type(element_type) && !is_f64_type(element_type) {
                return Err(syn::Error::new_spanned(
                    element_type,
                    "vector editor requires Vec<f32> or Vec<f64>",
                ));
            }
            require_numeric_range(field, metadata)?;
            if metadata.expected_len.is_none() {
                return Err(syn::Error::new_spanned(
                    field,
                    "vector editor requires len = ... or expected_len = ...",
                ));
            }
            Ok(AccessKind::NumericVector(element_type.clone()))
        }
        EditorMetadata::ReadOnly => infer_read_only_access_kind(field, metadata),
    }
}

fn infer_select_access_kind(
    field: &Field,
    metadata: &CompleteFieldMetadata,
) -> syn::Result<AccessKind> {
    if metadata.option_provider.is_none() && metadata.options.is_empty() {
        return Err(syn::Error::new_spanned(
            field,
            "select editor requires option_provider = ... or options(...)",
        ));
    }

    if is_string_type(&field.ty) {
        return Ok(AccessKind::SelectString);
    }

    let all_options_have_values = metadata
        .options
        .iter()
        .all(|option| option.value_path.is_some());
    if !metadata.options.is_empty() && all_options_have_values {
        return Ok(AccessKind::SelectEnum(metadata.options.clone()));
    }

    Err(syn::Error::new_spanned(
        field,
        "select editor for non-String fields requires options(option(id = ..., value = Type::Variant), ...)",
    ))
}

fn infer_select_list_access_kind(
    field: &Field,
    metadata: &CompleteFieldMetadata,
) -> syn::Result<AccessKind> {
    if metadata.option_provider.is_some() {
        return Err(syn::Error::new_spanned(
            field,
            "select_list editor currently supports only static options",
        ));
    }
    if metadata.options.is_empty() {
        return Err(syn::Error::new_spanned(
            field,
            "select_list editor requires options(...)",
        ));
    }

    let Some(element_type) = vec_element_type(&field.ty) else {
        return Err(syn::Error::new_spanned(
            field,
            "select_list editor requires Vec<String> or Vec<Enum>",
        ));
    };

    if is_string_type(element_type) {
        return Ok(AccessKind::SelectListString);
    }

    let all_options_have_values = metadata
        .options
        .iter()
        .all(|option| option.value_path.is_some());
    if all_options_have_values {
        return Ok(AccessKind::SelectListEnum(metadata.options.clone()));
    }

    Err(syn::Error::new_spanned(
        field,
        "select_list editor for Vec<Enum> requires options(option(id = ..., value = Type::Variant), ...)",
    ))
}

fn infer_read_only_access_kind(
    field: &Field,
    metadata: &CompleteFieldMetadata,
) -> syn::Result<AccessKind> {
    if is_bool_type(&field.ty) {
        Ok(AccessKind::Bool)
    } else if is_integer_type(&field.ty) {
        Ok(AccessKind::Integer)
    } else if is_float_type(&field.ty) {
        Ok(AccessKind::Float)
    } else if is_string_type(&field.ty) {
        Ok(AccessKind::Text)
    } else if let Some(element_type) = vec_element_type(&field.ty) {
        if !is_f32_type(element_type) && !is_f64_type(element_type) {
            return Err(syn::Error::new_spanned(
                element_type,
                "read_only vector fields must be Vec<f32> or Vec<f64>",
            ));
        }
        Ok(AccessKind::NumericVector(element_type.clone()))
    } else if !metadata.options.is_empty()
        && metadata
            .options
            .iter()
            .all(|option| option.value_path.is_some())
    {
        Ok(AccessKind::SelectEnum(metadata.options.clone()))
    } else {
        Err(syn::Error::new_spanned(
            field,
            "read_only fields require bool, integer, float, String, Vec<f32>, Vec<f64>, or select options with value paths",
        ))
    }
}

fn require_type_kind(
    field: &Field,
    predicate: fn(&Type) -> bool,
    message: &str,
) -> syn::Result<()> {
    if predicate(&field.ty) {
        Ok(())
    } else {
        Err(syn::Error::new_spanned(field, message))
    }
}

fn require_numeric_range(field: &Field, metadata: &CompleteFieldMetadata) -> syn::Result<()> {
    if metadata.min.is_none() || metadata.max.is_none() {
        return Err(syn::Error::new_spanned(
            field,
            "numeric/vector editor requires min = ... and max = ...",
        ));
    }
    Ok(())
}

fn type_ident(type_: &Type) -> Option<&Ident> {
    let Type::Path(type_path) = type_ else {
        return None;
    };
    if type_path.qself.is_some() {
        return None;
    }
    type_path.path.segments.last().map(|segment| &segment.ident)
}

fn is_named_type(type_: &Type, expected: &str) -> bool {
    type_ident(type_).is_some_and(|ident| ident == expected)
}

fn is_bool_type(type_: &Type) -> bool {
    is_named_type(type_, "bool")
}

fn is_string_type(type_: &Type) -> bool {
    is_named_type(type_, "String")
}

fn is_f32_type(type_: &Type) -> bool {
    is_named_type(type_, "f32")
}

fn is_f64_type(type_: &Type) -> bool {
    is_named_type(type_, "f64")
}

fn is_float_type(type_: &Type) -> bool {
    is_f32_type(type_) || is_f64_type(type_)
}

fn is_integer_type(type_: &Type) -> bool {
    type_ident(type_).is_some_and(|ident| {
        matches!(
            ident.to_string().as_str(),
            "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize"
        )
    })
}

fn vec_element_type(type_: &Type) -> Option<&Type> {
    let Type::Path(type_path) = type_ else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Vec" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    let first_argument = arguments.args.first()?;
    let syn::GenericArgument::Type(element_type) = first_argument else {
        return None;
    };
    Some(element_type)
}

fn expand_nested_field(field_ident: &Ident, field_type: &Type) -> TokenStream2 {
    quote! {
        registry.extend(
            <#field_type as ::settings_core::SettingsSchema>::settings_registry()?
                .map_document(
                    |document: &Self| &document.#field_ident,
                    |document: &mut Self| &mut document.#field_ident,
                )
        )?;
    }
}

fn expand_setting_registration(
    accessor_name: &Ident,
    metadata: &CompleteFieldMetadata,
    access_kind: &AccessKind,
) -> syn::Result<TokenStream2> {
    let descriptor = expand_descriptor(metadata, access_kind)?;

    Ok(quote! {
        registry.register(#descriptor, #accessor_name)?;
    })
}

fn expand_descriptor(
    metadata: &CompleteFieldMetadata,
    access_kind: &AccessKind,
) -> syn::Result<TokenStream2> {
    let id = &metadata.id;
    let path = &metadata.path;
    let section = &metadata.section;
    let group = &metadata.group;
    let surface = &metadata.surface;
    let text = expand_descriptor_text(metadata)?;
    let value_type = value_type_tokens(access_kind);
    let editor = editor_tokens(metadata, access_kind)?;
    let access = if metadata.read_only {
        quote! { ::settings_core::SettingAccess::ReadOnly }
    } else {
        quote! { ::settings_core::SettingAccess::ReadWrite }
    };
    let default_behavior = default_behavior_tokens(metadata.default_behavior);
    let (route, apply_mode) = apply_tokens(metadata);

    Ok(quote! {
        ::settings_core::SettingDescriptor {
            id: ::settings_core::SettingId::from(#id),
            path: ::settings_core::SettingPath::from(#path),
            text: #text,
            placement: ::settings_core::SettingPlacement::new(#section, #group, #surface),
            value_type: #value_type,
            editor: #editor,
            access: #access,
            default_behavior: #default_behavior,
            route: ::settings_core::SettingRouteId::from(#route),
            apply_mode: #apply_mode,
        }
    })
}

fn expand_descriptor_text(metadata: &CompleteFieldMetadata) -> syn::Result<TokenStream2> {
    let label_id = &metadata.label_id;
    let label_ru = &metadata.label_ru;
    let mut text = quote! {
        ::settings_core::SettingDescriptorText::new(
            ::settings_core::SettingText::new(#label_id, #label_ru)
        )
    };

    match (&metadata.description_id, &metadata.description_ru) {
        (Some(description_id), Some(description_ru)) => {
            text = quote! {
                #text.with_description(
                    ::settings_core::SettingText::new(#description_id, #description_ru)
                )
            };
        }
        (None, None) => {}
        _ => {
            return Err(syn::Error::new_spanned(
                &metadata.id,
                "description_id and description_ru must be provided together",
            ));
        }
    }

    match (&metadata.help_id, &metadata.help_ru) {
        (Some(help_id), Some(help_ru)) => {
            text = quote! {
                #text.with_help(::settings_core::SettingText::new(#help_id, #help_ru))
            };
        }
        (None, None) => {}
        _ => {
            return Err(syn::Error::new_spanned(
                &metadata.id,
                "help_id and help_ru must be provided together",
            ));
        }
    }

    Ok(text)
}

fn value_type_tokens(access_kind: &AccessKind) -> TokenStream2 {
    match access_kind {
        AccessKind::Bool => quote! { ::settings_core::SettingValueType::Bool },
        AccessKind::Integer => quote! { ::settings_core::SettingValueType::Integer },
        AccessKind::Float => quote! { ::settings_core::SettingValueType::Float },
        AccessKind::Text => quote! { ::settings_core::SettingValueType::Text },
        AccessKind::SelectString | AccessKind::SelectEnum(_) => {
            quote! { ::settings_core::SettingValueType::Select }
        }
        AccessKind::SelectListString | AccessKind::SelectListEnum(_) => {
            quote! { ::settings_core::SettingValueType::SelectList }
        }
        AccessKind::NumericVector(_) => {
            quote! { ::settings_core::SettingValueType::NumericVector }
        }
    }
}

fn editor_tokens(
    metadata: &CompleteFieldMetadata,
    access_kind: &AccessKind,
) -> syn::Result<TokenStream2> {
    match metadata.editor {
        EditorMetadata::Toggle => Ok(quote! { ::settings_core::SettingEditor::Toggle }),
        EditorMetadata::Integer | EditorMetadata::Float => {
            let numeric_descriptor = numeric_descriptor_tokens(metadata, access_kind)?;
            Ok(quote! { ::settings_core::SettingEditor::Numeric(#numeric_descriptor) })
        }
        EditorMetadata::Select => {
            let select_descriptor = select_descriptor_tokens(metadata)?;
            Ok(quote! { ::settings_core::SettingEditor::Select(#select_descriptor) })
        }
        EditorMetadata::SelectList => {
            let select_list_descriptor = select_list_descriptor_tokens(metadata)?;
            Ok(quote! { ::settings_core::SettingEditor::SelectList(#select_list_descriptor) })
        }
        EditorMetadata::Text => {
            let text_descriptor = text_descriptor_tokens(metadata);
            Ok(quote! { ::settings_core::SettingEditor::Text(#text_descriptor) })
        }
        EditorMetadata::Vector => {
            let numeric_descriptor = numeric_descriptor_tokens(metadata, access_kind)?;
            let labels = vector_label_tokens(metadata);
            let expected_len = metadata.expected_len.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(&metadata.id, "vector editor requires expected_len")
            })?;
            Ok(quote! {
                ::settings_core::SettingEditor::Vector(
                    ::settings_core::VectorDescriptor::new(
                        #numeric_descriptor,
                        vec![#(#labels),*],
                        (#expected_len) as usize,
                    )
                )
            })
        }
        EditorMetadata::ReadOnly => Ok(quote! { ::settings_core::SettingEditor::ReadOnly }),
    }
}

fn numeric_descriptor_tokens(
    metadata: &CompleteFieldMetadata,
    access_kind: &AccessKind,
) -> syn::Result<TokenStream2> {
    let min = metadata.min.as_ref().ok_or_else(|| {
        syn::Error::new_spanned(&metadata.id, "numeric editor requires min = ...")
    })?;
    let max = metadata.max.as_ref().ok_or_else(|| {
        syn::Error::new_spanned(&metadata.id, "numeric editor requires max = ...")
    })?;
    let unit = metadata
        .unit
        .as_ref()
        .map_or_else(|| quote! { None }, |unit| quote! { Some(#unit.into()) });

    let descriptor = match access_kind {
        AccessKind::Integer => {
            let step = metadata
                .step
                .as_ref()
                .map_or_else(|| quote! { 1_i64 }, |step| quote! { (#step) as i64 });
            quote! {
                ::settings_core::NumericDescriptor::new(
                    ::settings_core::NumericRange::Integer {
                        min: (#min) as i64,
                        max: (#max) as i64,
                    },
                    ::settings_core::NumericStep::Integer(#step),
                    #unit,
                )
            }
        }
        AccessKind::Float | AccessKind::NumericVector(_) => {
            let step = metadata
                .step
                .as_ref()
                .map_or_else(|| quote! { 0.1_f64 }, |step| quote! { (#step) as f64 });
            quote! {
                ::settings_core::NumericDescriptor::new(
                    ::settings_core::NumericRange::Float {
                        min: (#min) as f64,
                        max: (#max) as f64,
                    },
                    ::settings_core::NumericStep::Float(#step),
                    #unit,
                )
            }
        }
        _ => {
            return Err(syn::Error::new_spanned(
                &metadata.id,
                "numeric descriptor can be generated only for integer, float or vector fields",
            ));
        }
    };

    Ok(descriptor)
}

fn select_descriptor_tokens(metadata: &CompleteFieldMetadata) -> syn::Result<TokenStream2> {
    if let Some(provider_id) = &metadata.option_provider {
        return Ok(quote! {
            ::settings_core::SelectDescriptor::Dynamic {
                provider_id: ::settings_core::OptionProviderId::from(#provider_id),
            }
        });
    }

    let options = metadata.options.iter().map(|option| {
        let id = &option.id;
        let label_id = option.label_id.as_ref().unwrap_or(id);
        let label_ru = option.label_ru.as_ref().unwrap_or(id);
        quote! {
            ::settings_core::SettingOption::new(
                #id,
                ::settings_core::SettingText::new(#label_id, #label_ru),
            )
        }
    });

    Ok(quote! {
        ::settings_core::SelectDescriptor::Static {
            options: vec![#(#options),*],
        }
    })
}

fn select_list_descriptor_tokens(metadata: &CompleteFieldMetadata) -> syn::Result<TokenStream2> {
    if metadata.option_provider.is_some() {
        return Err(syn::Error::new_spanned(
            &metadata.id,
            "select_list editor currently supports only static options",
        ));
    }

    let options = metadata.options.iter().map(|option| {
        let id = &option.id;
        let label_id = option.label_id.as_ref().unwrap_or(id);
        let label_ru = option.label_ru.as_ref().unwrap_or(id);
        quote! {
            ::settings_core::SettingOption::new(
                #id,
                ::settings_core::SettingText::new(#label_id, #label_ru),
            )
        }
    });

    let mut descriptor = quote! {
        ::settings_core::SelectListDescriptor::new(vec![#(#options),*])
    };
    if let Some(min_len) = &metadata.min_len {
        descriptor = quote! { #descriptor.with_min_len((#min_len) as usize) };
    }
    if let Some(max_len) = &metadata.max_len {
        descriptor = quote! { #descriptor.with_max_len((#max_len) as usize) };
    }

    Ok(descriptor)
}

fn text_descriptor_tokens(metadata: &CompleteFieldMetadata) -> TokenStream2 {
    let format = match metadata.text_format {
        TextFormatMetadata::SingleLine => quote! { ::settings_core::TextFormat::SingleLine },
        TextFormatMetadata::Multiline => quote! { ::settings_core::TextFormat::Multiline },
    };
    let mut descriptor = quote! {
        ::settings_core::TextDescriptor::new(#format)
    };
    if let Some(min_len) = &metadata.min_len {
        descriptor = quote! { #descriptor.with_min_len((#min_len) as usize) };
    }
    if let Some(max_len) = &metadata.max_len {
        descriptor = quote! { #descriptor.with_max_len((#max_len) as usize) };
    }
    descriptor
}

fn vector_label_tokens(metadata: &CompleteFieldMetadata) -> Vec<TokenStream2> {
    metadata
        .vector_labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let text_id = format!("{}.vector_label.{index}", metadata.id.value());
            quote! { ::settings_core::SettingText::new(#text_id, #label) }
        })
        .collect()
}

fn default_behavior_tokens(default_behavior: DefaultMetadata) -> TokenStream2 {
    match default_behavior {
        DefaultMetadata::FromDefaultDocument => {
            quote! { ::settings_core::DefaultBehavior::FromDefaultDocument }
        }
        DefaultMetadata::NoReset => quote! { ::settings_core::DefaultBehavior::NoReset },
    }
}

fn apply_tokens(metadata: &CompleteFieldMetadata) -> (String, TokenStream2) {
    let apply = metadata.apply.value();
    let (derived_route, apply_mode) = if let Some(route) = apply.strip_suffix(".preview") {
        (
            route.to_owned(),
            quote! { ::settings_core::SettingApplyMode::ImmediatePreview },
        )
    } else if let Some(route) = apply.strip_suffix(".immediate_preview") {
        (
            route.to_owned(),
            quote! { ::settings_core::SettingApplyMode::ImmediatePreview },
        )
    } else if apply == "preview" || apply == "immediate_preview" {
        (
            apply.clone(),
            quote! { ::settings_core::SettingApplyMode::ImmediatePreview },
        )
    } else if let Some(route) = apply.strip_suffix(".committed") {
        (
            route.to_owned(),
            quote! { ::settings_core::SettingApplyMode::CommittedApply },
        )
    } else if let Some(route) = apply.strip_suffix(".apply") {
        (
            route.to_owned(),
            quote! { ::settings_core::SettingApplyMode::CommittedApply },
        )
    } else {
        (
            apply.clone(),
            quote! { ::settings_core::SettingApplyMode::CommittedApply },
        )
    };

    let route = metadata.route.as_ref().map_or(derived_route, LitStr::value);
    (route, apply_mode)
}

struct AccessorExpansion<'a> {
    struct_name: &'a Ident,
    impl_generics: &'a TokenStream2,
    type_generics: &'a TokenStream2,
    where_clause: &'a TokenStream2,
    accessor_name: &'a Ident,
    field_ident: &'a Ident,
    field_type: &'a Type,
    access_kind: &'a AccessKind,
    setting_id: String,
}

fn expand_accessor_impl(input: AccessorExpansion<'_>) -> TokenStream2 {
    let struct_name = input.struct_name;
    let impl_generics = input.impl_generics;
    let type_generics = input.type_generics;
    let where_clause = input.where_clause;
    let accessor_name = input.accessor_name;
    let get_body = accessor_get_body(input.field_ident, input.access_kind);
    let set_body = accessor_set_body(
        input.field_ident,
        input.field_type,
        input.access_kind,
        &input.setting_id,
    );
    let reset_body = accessor_reset_body(input.field_ident);

    quote! {
        struct #accessor_name;

        impl #impl_generics ::settings_core::SettingAccessor<#struct_name #type_generics> for #accessor_name #where_clause {
            fn get(&self, document: &#struct_name #type_generics) -> ::settings_core::SettingsResult<::settings_core::SettingValue> {
                #get_body
            }

            fn set(
                &self,
                document: &mut #struct_name #type_generics,
                value: ::settings_core::SettingValue,
            ) -> ::settings_core::SettingsResult<()> {
                #set_body
            }

            fn reset(
                &self,
                document: &mut #struct_name #type_generics,
                default_document: &#struct_name #type_generics,
            ) -> ::settings_core::SettingsResult<()> {
                #reset_body
            }
        }
    }
}

fn accessor_get_body(field_ident: &Ident, access_kind: &AccessKind) -> TokenStream2 {
    match access_kind {
        AccessKind::Bool => quote! {
            Ok(::settings_core::SettingValue::Bool(document.#field_ident))
        },
        AccessKind::Integer => quote! {
            let setting_value = ::core::convert::TryFrom::try_from(document.#field_ident)
                .map_err(|_| ::settings_core::SettingsError::access_failed(
                    concat!(stringify!(#field_ident), " does not fit into i64")
                ))?;
            Ok(::settings_core::SettingValue::Integer(setting_value))
        },
        AccessKind::Float => quote! {
            Ok(::settings_core::SettingValue::Float(::core::convert::Into::<f64>::into(document.#field_ident)))
        },
        AccessKind::Text => quote! {
            Ok(::settings_core::SettingValue::Text(document.#field_ident.clone()))
        },
        AccessKind::SelectString => quote! {
            Ok(::settings_core::SettingValue::Select(
                ::settings_core::SettingOptionId::from(document.#field_ident.clone())
            ))
        },
        AccessKind::SelectEnum(options) => {
            let arms = options.iter().map(|option| {
                let option_id = &option.id;
                let value_path = option
                    .value_path
                    .as_ref()
                    .expect("select enum options are validated before codegen");
                quote! {
                    &#value_path => Ok(::settings_core::SettingValue::Select(
                        ::settings_core::SettingOptionId::from(#option_id)
                    )),
                }
            });
            quote! {
                match &document.#field_ident {
                    #(#arms)*
                }
            }
        }
        AccessKind::SelectListString => quote! {
            Ok(::settings_core::SettingValue::SelectList(
                document.#field_ident
                    .iter()
                    .cloned()
                    .map(::settings_core::SettingOptionId::from)
                    .collect()
            ))
        },
        AccessKind::SelectListEnum(options) => {
            let arms = options.iter().map(|option| {
                let option_id = &option.id;
                let value_path = option
                    .value_path
                    .as_ref()
                    .expect("select list enum options are validated before codegen");
                quote! {
                    &#value_path => ::core::result::Result::Ok(
                        ::settings_core::SettingOptionId::from(#option_id)
                    ),
                }
            });
            quote! {
                let mut selected_options = Vec::with_capacity(document.#field_ident.len());
                for selected_value in &document.#field_ident {
                    let selected_option = match selected_value {
                        #(#arms)*
                    }?;
                    selected_options.push(selected_option);
                }
                Ok(::settings_core::SettingValue::SelectList(selected_options))
            }
        }
        AccessKind::NumericVector(_) => quote! {
            Ok(::settings_core::SettingValue::NumericVector(
                document.#field_ident.iter().copied().map(::core::convert::Into::<f64>::into).collect()
            ))
        },
    }
}

fn accessor_set_body(
    field_ident: &Ident,
    field_type: &Type,
    access_kind: &AccessKind,
    setting_id: &str,
) -> TokenStream2 {
    let expected_message = format!("{setting_id} expected {}", setting_value_name(access_kind));
    match access_kind {
        AccessKind::Bool => quote! {
            let ::settings_core::SettingValue::Bool(setting_value) = value else {
                return Err(::settings_core::SettingsError::access_failed(#expected_message));
            };
            document.#field_ident = setting_value;
            Ok(())
        },
        AccessKind::Integer => quote! {
            let ::settings_core::SettingValue::Integer(setting_value) = value else {
                return Err(::settings_core::SettingsError::access_failed(#expected_message));
            };
            document.#field_ident = <#field_type as ::core::convert::TryFrom<i64>>::try_from(setting_value)
                .map_err(|_| ::settings_core::SettingsError::access_failed(
                    format!("{} cannot store integer {}", #setting_id, setting_value)
                ))?;
            Ok(())
        },
        AccessKind::Float => quote! {
            let ::settings_core::SettingValue::Float(setting_value) = value else {
                return Err(::settings_core::SettingsError::access_failed(#expected_message));
            };
            document.#field_ident = setting_value as #field_type;
            Ok(())
        },
        AccessKind::Text => quote! {
            let ::settings_core::SettingValue::Text(setting_value) = value else {
                return Err(::settings_core::SettingsError::access_failed(#expected_message));
            };
            document.#field_ident = setting_value;
            Ok(())
        },
        AccessKind::SelectString => quote! {
            let ::settings_core::SettingValue::Select(setting_value) = value else {
                return Err(::settings_core::SettingsError::access_failed(#expected_message));
            };
            document.#field_ident = setting_value.into_string();
            Ok(())
        },
        AccessKind::SelectEnum(options) => {
            let arms = options.iter().map(|option| {
                let option_id = &option.id;
                let value_path = option
                    .value_path
                    .as_ref()
                    .expect("select enum options are validated before codegen");
                quote! { #option_id => #value_path, }
            });
            quote! {
                let ::settings_core::SettingValue::Select(setting_value) = value else {
                    return Err(::settings_core::SettingsError::access_failed(#expected_message));
                };
                document.#field_ident = match setting_value.as_str() {
                    #(#arms)*
                    _ => {
                        return Err(::settings_core::SettingsError::access_failed(
                            format!("{} cannot map unknown option {}", #setting_id, setting_value)
                        ));
                    }
                };
                Ok(())
            }
        }
        AccessKind::SelectListString => quote! {
            let ::settings_core::SettingValue::SelectList(setting_value) = value else {
                return Err(::settings_core::SettingsError::access_failed(#expected_message));
            };
            document.#field_ident = setting_value
                .into_iter()
                .map(::settings_core::SettingOptionId::into_string)
                .collect();
            Ok(())
        },
        AccessKind::SelectListEnum(options) => {
            let arms = options.iter().map(|option| {
                let option_id = &option.id;
                let value_path = option
                    .value_path
                    .as_ref()
                    .expect("select list enum options are validated before codegen");
                quote! { #option_id => #value_path, }
            });
            quote! {
                let ::settings_core::SettingValue::SelectList(setting_value) = value else {
                    return Err(::settings_core::SettingsError::access_failed(#expected_message));
                };
                let mut selected_values = Vec::with_capacity(setting_value.len());
                for selected_option in setting_value {
                    let selected_value = match selected_option.as_str() {
                        #(#arms)*
                        _ => {
                            return Err(::settings_core::SettingsError::access_failed(
                                format!("{} cannot map unknown option {}", #setting_id, selected_option)
                            ));
                        }
                    };
                    selected_values.push(selected_value);
                }
                document.#field_ident = selected_values;
                Ok(())
            }
        }
        AccessKind::NumericVector(element_type) => quote! {
            let ::settings_core::SettingValue::NumericVector(setting_value) = value else {
                return Err(::settings_core::SettingsError::access_failed(#expected_message));
            };
            document.#field_ident = setting_value
                .into_iter()
                .map(|number| number as #element_type)
                .collect();
            Ok(())
        },
    }
}

fn accessor_reset_body(field_ident: &Ident) -> TokenStream2 {
    quote! {
        document.#field_ident.clone_from(&default_document.#field_ident);
        Ok(())
    }
}

fn setting_value_name(access_kind: &AccessKind) -> &'static str {
    match access_kind {
        AccessKind::Bool => "bool",
        AccessKind::Integer => "integer",
        AccessKind::Float => "float",
        AccessKind::Text => "text",
        AccessKind::SelectString | AccessKind::SelectEnum(_) => "select",
        AccessKind::SelectListString | AccessKind::SelectListEnum(_) => "select list",
        AccessKind::NumericVector(_) => "numeric vector",
    }
}
