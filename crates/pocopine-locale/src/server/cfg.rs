use std::collections::{BTreeMap, BTreeSet};

use syn::{Meta, Token, parse::Parser, punctuated::Punctuated};

/// The actual compilation target's `rustc --print cfg` output plus enabled
/// Cargo features and explicitly supplied build-script flags. Never use the
/// extraction process's own target to decide browser reachability.
#[derive(Clone, Debug, Default)]
pub struct CfgSet {
    flags: BTreeSet<String>,
    values: BTreeMap<String, BTreeSet<String>>,
}

impl CfgSet {
    pub fn from_rustc(output: &str) -> Result<Self, String> {
        let mut set = Self::default();
        for line in output.lines().map(str::trim).filter(|s| !s.is_empty()) {
            let meta: Meta =
                syn::parse_str(line).map_err(|e| format!("invalid rustc cfg {line:?}: {e}"))?;
            match meta {
                Meta::Path(path) => set.insert_flag(
                    path.get_ident()
                        .ok_or("cfg flag must be an identifier")?
                        .to_string(),
                ),
                Meta::NameValue(pair) => {
                    let name = pair
                        .path
                        .get_ident()
                        .ok_or("cfg key must be an identifier")?
                        .to_string();
                    let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(value),
                        ..
                    }) = pair.value
                    else {
                        return Err("cfg value must be a string".into());
                    };
                    set.insert_value(name, value.value());
                }
                _ => return Err("rustc cfg output must contain flags or key/value pairs".into()),
            }
        }
        Ok(set)
    }

    pub fn insert_flag(&mut self, flag: impl Into<String>) {
        self.flags.insert(flag.into());
    }
    pub fn insert_value(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values
            .entry(key.into())
            .or_default()
            .insert(value.into());
    }

    pub fn matches(&self, predicate: &Meta) -> Result<bool, String> {
        match predicate {
            Meta::Path(path) => Ok(self.flags.contains(
                &path
                    .get_ident()
                    .ok_or("cfg flag must be an identifier")?
                    .to_string(),
            )),
            Meta::NameValue(pair) => {
                let key = pair
                    .path
                    .get_ident()
                    .ok_or("cfg key must be an identifier")?
                    .to_string();
                let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(value),
                    ..
                }) = &pair.value
                else {
                    return Err("cfg value must be a string".into());
                };
                Ok(self
                    .values
                    .get(&key)
                    .is_some_and(|values| values.contains(&value.value())))
            }
            Meta::List(list) => {
                let args = Punctuated::<Meta, Token![,]>::parse_terminated
                    .parse2(list.tokens.clone())
                    .map_err(|e| e.to_string())?;
                let values = args
                    .iter()
                    .map(|arg| self.matches(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                if list.path.is_ident("all") {
                    Ok(values.iter().all(|v| *v))
                } else if list.path.is_ident("any") {
                    Ok(values.iter().any(|v| *v))
                } else if list.path.is_ident("not") && values.len() == 1 {
                    Ok(!values[0])
                } else {
                    Err("cfg supports all(...), any(...), and not(one_predicate)".into())
                }
            }
        }
    }

    /// Expand active cfg_attr wrappers and return None for a cfg-disabled
    /// item. Attribute order does not change the result.
    pub fn attributes(&self, attrs: &[syn::Attribute]) -> Result<Option<Vec<Meta>>, String> {
        let mut result = Vec::new();
        for attribute in attrs {
            self.expand(&attribute.meta, &mut result, 0)?;
        }
        for meta in &result {
            if let Meta::List(list) = meta
                && list.path.is_ident("cfg")
            {
                let predicate: Meta =
                    syn::parse2(list.tokens.clone()).map_err(|e| e.to_string())?;
                if !self.matches(&predicate)? {
                    return Ok(None);
                }
            }
        }
        Ok(Some(result))
    }

    fn expand(&self, meta: &Meta, out: &mut Vec<Meta>, depth: usize) -> Result<(), String> {
        if depth > 32 {
            return Err("cfg_attr nesting exceeds 32 levels".into());
        }
        if let Meta::List(list) = meta
            && list.path.is_ident("cfg_attr")
        {
            let mut args = Punctuated::<Meta, Token![,]>::parse_terminated
                .parse2(list.tokens.clone())
                .map_err(|e| e.to_string())?
                .into_iter();
            let predicate = args.next().ok_or("cfg_attr requires a predicate")?;
            if self.matches(&predicate)? {
                for arg in args {
                    self.expand(&arg, out, depth + 1)?;
                }
            }
        } else {
            out.push(meta.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_and_feature_conditions_include_nested_cfg_attr() {
        let mut cfg =
            CfgSet::from_rustc("target_arch=\"wasm32\"\ntarget_pointer_width=\"32\"").unwrap();
        cfg.insert_flag("pocopine_browser");
        cfg.insert_value("feature", "email");
        for source in [
            "all(target_arch=\"wasm32\",feature=\"email\")",
            "any(unset, pocopine_browser)",
            "not(pocopine_host)",
        ] {
            assert!(cfg.matches(&syn::parse_str(source).unwrap()).unwrap());
        }
        let item: syn::ItemFn = syn::parse_quote! { #[cfg_attr(feature="email", cfg(not(target_arch="wasm32")))] fn f() {} };
        assert!(cfg.attributes(&item.attrs).unwrap().is_none());
        assert!(cfg.matches(&syn::parse_quote!(not(a, b))).is_err());
    }
}
