//! Генерация registry, descriptor-ов и typed accessors из проверенной metadata.

mod parsing;
mod validation;

use proc_macro2::{Ident, TokenStream as TokenStream2};
use quote::quote;
use syn::{DeriveInput, LitStr, Type};

use parsing::{
    CompleteFieldMetadata, DefaultMetadata, EditorMetadata, FieldConfig, TextFormatMetadata,
    accessor_ident, named_struct_fields, parse_field_config, parse_struct_settings,
};
use validation::{AccessKind, infer_access_kind};

pub(super) fn expand_settings_schema(input: DeriveInput) -> syn::Result<TokenStream2> {
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
