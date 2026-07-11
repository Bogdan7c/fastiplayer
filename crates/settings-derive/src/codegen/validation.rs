//! Семантическая проверка совместимости editor metadata и Rust field types.

use proc_macro2::Ident;
use syn::{Field, Type};

use super::parsing::{CompleteFieldMetadata, EditorMetadata, SelectOptionMetadata};

pub(super) enum AccessKind {
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

pub(super) fn infer_access_kind(
    field: &Field,
    metadata: &CompleteFieldMetadata,
) -> syn::Result<AccessKind> {
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

pub(super) fn infer_select_access_kind(
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

pub(super) fn infer_select_list_access_kind(
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

pub(super) fn infer_read_only_access_kind(
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

pub(super) fn require_type_kind(
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

pub(super) fn require_numeric_range(
    field: &Field,
    metadata: &CompleteFieldMetadata,
) -> syn::Result<()> {
    if metadata.min.is_none() || metadata.max.is_none() {
        return Err(syn::Error::new_spanned(
            field,
            "numeric/vector editor requires min = ... and max = ...",
        ));
    }
    Ok(())
}

pub(super) fn type_ident(type_: &Type) -> Option<&Ident> {
    let Type::Path(type_path) = type_ else {
        return None;
    };
    if type_path.qself.is_some() {
        return None;
    }
    type_path.path.segments.last().map(|segment| &segment.ident)
}

pub(super) fn is_named_type(type_: &Type, expected: &str) -> bool {
    type_ident(type_).is_some_and(|ident| ident == expected)
}

pub(super) fn is_bool_type(type_: &Type) -> bool {
    is_named_type(type_, "bool")
}

pub(super) fn is_string_type(type_: &Type) -> bool {
    is_named_type(type_, "String")
}

pub(super) fn is_f32_type(type_: &Type) -> bool {
    is_named_type(type_, "f32")
}

pub(super) fn is_f64_type(type_: &Type) -> bool {
    is_named_type(type_, "f64")
}

pub(super) fn is_float_type(type_: &Type) -> bool {
    is_f32_type(type_) || is_f64_type(type_)
}

pub(super) fn is_integer_type(type_: &Type) -> bool {
    type_ident(type_).is_some_and(|ident| {
        matches!(
            ident.to_string().as_str(),
            "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize"
        )
    })
}

pub(super) fn vec_element_type(type_: &Type) -> Option<&Type> {
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
