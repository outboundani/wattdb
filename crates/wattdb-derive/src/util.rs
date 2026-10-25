use proc_macro2::Span;
use syn::{GenericArgument, Ident, PathArguments, Type};

/// Splits an identifier into lowercase words.
fn words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' || c == ' ' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let boundary = c.is_uppercase()
            && !cur.is_empty()
            && (chars[i - 1].is_lowercase()
                || chars[i - 1].is_ascii_digit()
                || (i + 1 < chars.len() && chars[i + 1].is_lowercase()));
        if boundary {
            out.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn capitalize(w: &str) -> String {
    let mut c = w.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

pub fn pascal(s: &str) -> String {
    words(s).iter().map(|w| capitalize(w)).collect()
}

pub fn snake(s: &str) -> String {
    words(s).join("_")
}

pub fn camel(s: &str) -> String {
    let w = words(s);
    let mut out = String::new();
    for (i, x) in w.iter().enumerate() {
        out.push_str(&if i == 0 { x.clone() } else { capitalize(x) });
    }
    out
}

/// Applies a serde-style `rename_all` rule.
pub fn rename(rule: &str, s: &str) -> Option<String> {
    Some(match rule {
        "lowercase" => s.to_lowercase(),
        "UPPERCASE" => s.to_uppercase(),
        "camelCase" => camel(s),
        "PascalCase" => pascal(s),
        "snake_case" => snake(s),
        "SCREAMING_SNAKE_CASE" => snake(s).to_uppercase(),
        "kebab-case" => words(s).join("-"),
        "SCREAMING-KEBAB-CASE" => words(s).join("-").to_uppercase(),
        _ => return None,
    })
}

pub fn ident(s: &str) -> Ident {
    let s = if is_keyword(s) { format!("r#{s}") } else { s.to_string() };
    syn::parse_str(&s).unwrap_or_else(|_| Ident::new("__invalid", Span::call_site()))
}

fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        "as" | "break"
            | "const"
            | "continue"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "static"
            | "struct"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "async"
            | "await"
            | "dyn"
            | "abstract"
            | "become"
            | "box"
            | "do"
            | "final"
            | "macro"
            | "override"
            | "priv"
            | "typeof"
            | "unsized"
            | "virtual"
            | "yield"
            | "try"
            | "gen"
    )
}

/// The `T` of an `Option<T>`.
pub fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(p) = ty else { return None };
    if p.qself.is_some() {
        return None;
    }
    let seg = p.path.segments.last()?;
    if seg.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else { return None };
    match args.args.first()? {
        GenericArgument::Type(t) if args.args.len() == 1 => Some(t),
        _ => None,
    }
}

/// The `T` of a `Vec<T>`.
pub fn vec_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(p) = ty else { return None };
    let seg = p.path.segments.last()?;
    if seg.ident != "Vec" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else { return None };
    match args.args.first()? {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    }
}

/// A type path's last identifier, with the path before it.
pub fn err(span: Span, msg: impl std::fmt::Display) -> syn::Error {
    syn::Error::new(span, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cases() {
        assert_eq!(pascal("byAssignee"), "ByAssignee");
        assert_eq!(snake("byAssignee"), "by_assignee");
        assert_eq!(snake("orgMembers"), "org_members");
        assert_eq!(camel("org_id"), "orgId");
        assert_eq!(pascal("task_id"), "TaskId");
        assert_eq!(snake("byOwnerID"), "by_owner_id");
        assert_eq!(rename("lowercase", "InProgress").unwrap(), "inprogress");
        assert_eq!(rename("kebab-case", "InProgress").unwrap(), "in-progress");
    }
}

/// The `InCollection` id of a collection name (64-bit FNV-1a).
pub fn collection_id(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}
