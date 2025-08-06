use proc_macro::TokenStream;
use proc_macro2::Ident;
use quote::quote;
use syn::{Attribute, DeriveInput, parse_macro_input, spanned::Spanned};

#[proc_macro_derive(PartialModify, attributes(partial_modify_derive, partial_modify))]
pub fn partial_modify_derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let struct_name = &input.ident;
    let enum_name = Ident::new(&format!("{}Partial", struct_name), struct_name.span());

    let mut enum_derives = Vec::new();
    for attr in &input.attrs {
        let meta_attrs = match get_partial_modify_derivative_attribute(attr) {
            Ok(attrs) => attrs,
            Err(e) => return e.to_compile_error().into(),
        };
        for meta_attr in meta_attrs {
            enum_derives.push(meta_attr);
        }
    }
    let enum_derives = if enum_derives.is_empty() {
        // panic!();
        quote! {}
    } else {
        quote! { #[derive(#(#enum_derives),*)] }
    };

    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            syn::Fields::Named(named) => &named.named,
            _ => panic!("PartialModify can only be used with named fields"),
        },
        _ => panic!("PartialModify can only be used with structs"),
    };

    let mut enum_variants = Vec::new();
    let mut match_arms = Vec::new();

    for field in fields {
        let field_name = field.ident.as_ref().unwrap();

        let skip = field.attrs.iter().try_fold(false, |acc: bool, attr| {
            if acc {
                return Ok(true);
            }
            let ret = match is_skip(attr) {
                Ok(ret) => ret,
                Err(e) => return Err(e),
            };
            Ok(ret)
        });
        match skip {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => return e.to_compile_error().into(),
        }

        let variant_name = to_pascal_case(&field_name.to_string());
        let variant_ident = syn::Ident::new(&variant_name, field_name.span());
        let field_type = &field.ty;

        enum_variants.push(quote! {
            #variant_ident(#field_type)
        });

        match_arms.push(quote! {
            #enum_name::#variant_ident(val) => self.#field_name = val
        });
    }

    let expanded = quote! {
        #enum_derives
        pub enum #enum_name {
            #(#enum_variants),*
        }

        impl #struct_name {
            pub fn apply(&mut self, partial: #enum_name) {
                match partial {
                    #(#match_arms),*
                }
            }
        }
    };

    TokenStream::from(expanded)
}

fn get_partial_modify_derivative_attribute(
    attr: &Attribute,
) -> Result<Vec<Ident>, syn::Error> {
    let mut ret = Vec::new();
    if attr.path().is_ident("partial_modify_derive") {
        attr.parse_nested_meta(|meta| {
            ret.push(meta.path.get_ident().cloned().ok_or_else(|| {
                syn::Error::new(
                    meta.path.span(),
                    "Expected an identifier for partial_modify_derive attribute",
                )
            })?);
            Ok(())
        })?;
    } else if attr.path().is_ident("partial_modify") {
        return Err(syn::Error::new(
            attr.span(),
            "partial_modify should not be used as a derive attribute",
        ));
    } 
    Ok(ret)
}

fn is_skip(
    attr: &Attribute,
) -> Result<bool, syn::Error> {
    let mut ret = false;
    if attr.path().is_ident("partial_modify") {
        attr.parse_nested_meta(|meta| {
            ret = match meta.path.get_ident().cloned() {
                Some(ident) if ident == "Skip" => true,
                Some(_) => return Err(syn::Error::new(
                    meta.path.span(),
                    "Only Skip is allowed as a value for partial_modify attribute",
                )),
                None => return Err(syn::Error::new(
                    meta.path.span(),
                    "Expected an identifier for partial_modify attribute",
                )),
            };
            Ok(())
        })?;
    } else if attr.path().is_ident("partial_modify_derive") {
        return Err(syn::Error::new(
            attr.span(),
            "partial_modify_derive should not be used as a field attribute",
        ));
    }
    Ok(ret)
}

fn to_pascal_case(s: &str) -> String {
    use convert_case::{Case, Casing};
    s.to_case(Case::UpperCamel)
}
