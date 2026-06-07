//! Proc-macros for `rusm-rs`. `#[rusm_rs::service]` turns a module of free
//! functions into a RUSM service: it keeps the functions, and adds a `serve()`
//! dispatch loop (receive a request → call the matching function → reply) plus a
//! typed `Client` whose methods are blocking calls over the same JSON wire as
//! rusm-ts. Mirrors a TS service's `export function`s — no `impl` block, no `self`.

use proc_macro::TokenStream;
use quote::quote;
use syn::{FnArg, Ident, ItemFn, ItemMod, Pat, ReturnType, Type};

/// `#[rusm_rs::service]` on an inline `mod` of `pub fn`s.
#[proc_macro_attribute]
pub fn service(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let module = syn::parse_macro_input!(item as ItemMod);
    expand(module)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// One handler's parsed signature.
struct Handler {
    name: Ident,
    op: String,
    params: Vec<(Ident, Type)>,
    ret: proc_macro2::TokenStream,
}

fn expand(module: ItemMod) -> syn::Result<proc_macro2::TokenStream> {
    let content = module
        .content
        .as_ref()
        .ok_or_else(|| syn::Error::new_spanned(&module, "#[service] needs an inline module body"))?;
    let items = &content.1;

    let handlers: Vec<Handler> = items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Fn(f) => Some(parse_handler(f)),
            _ => None,
        })
        .collect::<syn::Result<_>>()?;

    let arms = handlers.iter().map(serve_arm);
    let methods = handlers.iter().map(client_method);

    let attrs = &module.attrs;
    let vis = &module.vis;
    let ident = &module.ident;
    Ok(quote! {
        #(#attrs)* #vis mod #ident {
            #(#items)*

            /// Run the request → dispatch → reply loop forever (a service's body).
            pub fn serve() -> ! {
                loop {
                    let req = rusm_rs::wire::next_request();
                    match req.op.as_str() {
                        #(#arms)*
                        other => rusm_rs::wire::reply_err(
                            &req, &format!("no such function: {}", other)),
                    }
                }
            }

            /// A typed, blocking client for this service.
            #[derive(Clone, Copy)]
            pub struct Client {
                pub pid: rusm_rs::Pid,
            }

            impl Client {
                /// Spawn a fresh instance of this service by name and connect to it.
                pub fn spawn(component: &str) -> ::core::result::Result<Self, ::std::string::String> {
                    ::core::result::Result::Ok(Self { pid: rusm_rs::spawn(component)? })
                }

                /// Connect to an already-running instance by pid.
                pub fn connect(pid: rusm_rs::Pid) -> Self {
                    Self { pid }
                }

                #(#methods)*
            }
        }
    })
}

fn parse_handler(f: &ItemFn) -> syn::Result<Handler> {
    let name = f.sig.ident.clone();
    let op = name.to_string();
    let mut params = Vec::new();
    for input in &f.sig.inputs {
        match input {
            FnArg::Receiver(r) => {
                return Err(syn::Error::new_spanned(
                    r,
                    "#[service] functions are free functions — no `self`",
                ))
            }
            FnArg::Typed(pt) => {
                let ident = match &*pt.pat {
                    Pat::Ident(p) => p.ident.clone(),
                    other => {
                        return Err(syn::Error::new_spanned(
                            other,
                            "parameters must be simple identifiers",
                        ))
                    }
                };
                params.push((ident, (*pt.ty).clone()));
            }
        }
    }
    let ret = match &f.sig.output {
        ReturnType::Default => quote!(()),
        ReturnType::Type(_, ty) => quote!(#ty),
    };
    Ok(Handler {
        name,
        op,
        params,
        ret,
    })
}

/// `&(a, b)` (args as a JSON array on the wire). `&[(); 0]` for no args, so it
/// serializes to `[]` (not `null`) — matching the TS wire.
fn args_tuple(idents: &[Ident]) -> proc_macro2::TokenStream {
    match idents.len() {
        0 => quote!(&[(); 0]),
        1 => {
            let a = &idents[0];
            quote!(&(#a,))
        }
        _ => quote!(&(#(#idents),*)),
    }
}

fn serve_arm(h: &Handler) -> proc_macro2::TokenStream {
    let name = &h.name;
    let op = &h.op;
    let idents: Vec<&Ident> = h.params.iter().map(|(i, _)| i).collect();
    if h.params.is_empty() {
        return quote! { #op => rusm_rs::wire::reply_ok(&req, &#name()), };
    }
    let types = h.params.iter().map(|(_, t)| t);
    // A 1-tuple needs the trailing comma: `(T,)` / `(a,)`.
    let (tuple_ty, binding) = if h.params.len() == 1 {
        let t = h.params[0].1.clone();
        let a = idents[0];
        (quote!((#t,)), quote!((#a,)))
    } else {
        (quote!((#(#types),*)), quote!((#(#idents),*)))
    };
    quote! {
        #op => match req.args::<#tuple_ty>() {
            ::core::result::Result::Ok(#binding) =>
                rusm_rs::wire::reply_ok(&req, &#name(#(#idents),*)),
            ::core::result::Result::Err(e) => rusm_rs::wire::reply_err(&req, &e),
        },
    }
}

fn client_method(h: &Handler) -> proc_macro2::TokenStream {
    let name = &h.name;
    let op = &h.op;
    let ret = &h.ret;
    let idents: Vec<&Ident> = h.params.iter().map(|(i, _)| i).collect();
    let types = h.params.iter().map(|(_, t)| t);
    let args = args_tuple(&idents.iter().map(|i| (*i).clone()).collect::<Vec<_>>());
    quote! {
        pub fn #name(&self #(, #idents: #types)*)
            -> ::core::result::Result<#ret, ::std::string::String> {
            rusm_rs::wire::call(self.pid, #op, #args)
        }
    }
}
