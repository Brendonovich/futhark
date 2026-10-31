//! Derives for operator parameters. They expand to paths in `futhark_graph`, which re-exports them.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as Tokens;
use quote::quote;
use syn::{Attribute, Data, DeriveInput, Expr, Fields, LitStr, parse_macro_input};

/// Builds the parameter schema of an operator from a struct.
///
/// Each field is a parameter named after the field. Its doc comment becomes the parameter's docs and its type sets the
/// parameter type (`f32`, `f64`, `i32`, `i64`, `bool`, `String`, or an enum deriving `Menu`). `#[param(...)]` takes
/// `default`, `label`, `min`, `max`, `soft_min` and `soft_max`.
#[proc_macro_derive(Params, attributes(param))]
pub fn derive_params(input: TokenStream) -> TokenStream {
    params(parse_macro_input!(input as DeriveInput))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Makes a fieldless enum usable as a menu parameter. Items are named after variants in snake case and labelled in
/// words; `#[menu(name = "...", label = "...")]` overrides either. The first variant is the default.
#[proc_macro_derive(Menu, attributes(menu))]
pub fn derive_menu(input: TokenStream) -> TokenStream {
    menu(parse_macro_input!(input as DeriveInput))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[derive(Default)]
struct ParamAttrs {
    default: Option<Expr>,
    label: Option<LitStr>,
    min: Option<Expr>,
    max: Option<Expr>,
    soft_min: Option<Expr>,
    soft_max: Option<Expr>,
}

impl ParamAttrs {
    fn parse(attrs: &[Attribute]) -> syn::Result<Self> {
        let mut out = ParamAttrs::default();
        for attr in attrs.iter().filter(|a| a.path().is_ident("param")) {
            attr.parse_nested_meta(|meta| {
                let slot = if meta.path.is_ident("default") {
                    &mut out.default
                } else if meta.path.is_ident("min") {
                    &mut out.min
                } else if meta.path.is_ident("max") {
                    &mut out.max
                } else if meta.path.is_ident("soft_min") {
                    &mut out.soft_min
                } else if meta.path.is_ident("soft_max") {
                    &mut out.soft_max
                } else if meta.path.is_ident("label") {
                    out.label = Some(meta.value()?.parse()?);
                    return Ok(());
                } else {
                    return Err(meta.error("expected default, label, min, max, soft_min or soft_max"));
                };
                *slot = Some(meta.value()?.parse()?);
                Ok(())
            })?;
        }
        Ok(out)
    }
}

fn params(input: DeriveInput) -> syn::Result<Tokens> {
    let name = &input.ident;
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(name, "Params can only be derived for structs"));
    };
    let (fields, construct) = match &data.fields {
        Fields::Named(fields) => (fields.named.iter().collect::<Vec<_>>(), true),
        Fields::Unit => (Vec::new(), false),
        Fields::Unnamed(_) => return Err(syn::Error::new_spanned(name, "Params needs named fields")),
    };

    let mut specs = Vec::new();
    let mut inits = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        let ident = field.ident.as_ref().expect("named field");
        let ty = &field.ty;
        let attrs = ParamAttrs::parse(&field.attrs)?;
        let param = ident.to_string().trim_start_matches("r#").to_string();
        let label = match attrs.label {
            Some(label) => label.value(),
            None => words(&param.split('_').map(capitalize).collect::<Vec<_>>()),
        };
        let doc = doc(&field.attrs);
        // Literals become values directly, so `default = 1` works for a float; `ParamSpec::normalized` fixes up the
        // variant. Anything else, such as a menu variant, must have the field's type.
        let default = match attrs.default.as_ref().map(|expr| (literal(expr), expr)) {
            Some((Some(syn::Lit::Float(_)), expr)) => quote!(::futhark_graph::Value::Float((#expr) as f64)),
            Some((Some(syn::Lit::Int(_)), expr)) => quote!(::futhark_graph::Value::Int((#expr) as i64)),
            Some((Some(syn::Lit::Str(_)), expr)) => {
                quote!(::futhark_graph::Value::String(::std::string::String::from(#expr)))
            }
            Some((_, expr)) => quote!(<#ty as ::futhark_graph::ParamType>::to_value(&(#expr))),
            None => quote!(<#ty as ::futhark_graph::ParamType>::zero()),
        };
        let bound = |expr: Option<Expr>| match expr {
            Some(expr) => quote!(::core::option::Option::Some((#expr) as f64)),
            None => quote!(::core::option::Option::None),
        };
        let (min, max, soft_min, soft_max) = (
            bound(attrs.min),
            bound(attrs.max),
            bound(attrs.soft_min),
            bound(attrs.soft_max),
        );
        specs.push(quote! {
            ::futhark_graph::ParamSpec {
                name: #param,
                label: #label,
                doc: #doc,
                kind: <#ty as ::futhark_graph::ParamType>::kind(),
                default: #default,
                min: #min,
                max: #max,
                soft_min: #soft_min,
                soft_max: #soft_max,
            }
            .normalized()
        });
        inits.push(quote!(#ident: <#ty as ::futhark_graph::ParamType>::from_value(&values[#index])));
    }

    let body = if construct {
        quote!(Self { #(#inits),* })
    } else {
        quote!(Self)
    };
    let values = if fields.is_empty() {
        quote!(_values)
    } else {
        quote!(values)
    };
    Ok(quote! {
        impl ::futhark_graph::Params for #name {
            fn specs() -> ::std::vec::Vec<::futhark_graph::ParamSpec> {
                ::std::vec![#(#specs),*]
            }

            fn from_values(#values: &[::futhark_graph::Value]) -> Self {
                #body
            }
        }
    })
}

fn menu(input: DeriveInput) -> syn::Result<Tokens> {
    let name = &input.ident;
    let Data::Enum(data) = &input.data else {
        return Err(syn::Error::new_spanned(name, "Menu can only be derived for enums"));
    };
    if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(name, "a menu needs at least one item"));
    }

    let mut items = Vec::new();
    let mut to_names = Vec::new();
    let mut from_names = Vec::new();
    for variant in &data.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(variant, "menu items can't have fields"));
        }
        let ident = &variant.ident;
        let mut item = snake_case(&ident.to_string());
        let mut label = words(&split_camel(&ident.to_string()));
        for attr in variant.attrs.iter().filter(|a| a.path().is_ident("menu")) {
            attr.parse_nested_meta(|meta| {
                let value: LitStr = meta.value()?.parse()?;
                if meta.path.is_ident("name") {
                    item = value.value();
                } else if meta.path.is_ident("label") {
                    label = value.value();
                } else {
                    return Err(meta.error("expected name or label"));
                }
                Ok(())
            })?;
        }
        items.push(quote!(::futhark_graph::MenuItem { name: #item, label: #label }));
        to_names.push(quote!(Self::#ident => #item));
        from_names.push(quote!(#item => Self::#ident));
    }
    let first = &data.variants[0].ident;
    let first_name = snake_case(&first.to_string());

    Ok(quote! {
        impl ::futhark_graph::ParamType for #name {
            fn kind() -> ::futhark_graph::ParamKind {
                ::futhark_graph::ParamKind::Menu(&[#(#items),*])
            }

            fn zero() -> ::futhark_graph::Value {
                ::futhark_graph::Value::Menu(::std::string::String::from(#first_name))
            }

            fn to_value(&self) -> ::futhark_graph::Value {
                ::futhark_graph::Value::Menu(::std::string::String::from(match self { #(#to_names),* }))
            }

            fn from_value(value: &::futhark_graph::Value) -> Self {
                match value {
                    ::futhark_graph::Value::Menu(name) => match name.as_str() {
                        #(#from_names,)*
                        _ => Self::#first,
                    },
                    _ => Self::#first,
                }
            }
        }
    })
}

/// The literal in `expr`, looking through a leading minus.
fn literal(expr: &Expr) -> Option<&syn::Lit> {
    match expr {
        Expr::Lit(lit) => Some(&lit.lit),
        Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Neg(_),
            expr,
            ..
        }) => literal(expr),
        Expr::Group(group) => literal(&group.expr),
        Expr::Paren(paren) => literal(&paren.expr),
        _ => None,
    }
}

fn doc(attrs: &[Attribute]) -> String {
    let lines: Vec<String> = attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            syn::Meta::NameValue(nv) => match &nv.value {
                Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s), ..
                }) => Some(s.value().trim().to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    lines.join("\n")
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn split_camel(ident: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for c in ident.chars() {
        match words.last_mut() {
            Some(word) if !c.is_uppercase() => word.push(c),
            _ => words.push(c.to_string()),
        }
    }
    words
}

fn words(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|w| !w.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

fn snake_case(ident: &str) -> String {
    split_camel(ident)
        .iter()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join("_")
}
