//! Resolves HTML hyperlinks against the acknowledged project inventory. A link becomes
//! an edge only when its target is a stored project page; every other link is dropped.
//! Links never become name references.

use crate::ScannedFile;
use crate::source::validate_path;
use std::collections::BTreeSet;

/// Resolution of one parsed link to a stored page. Example: `resolved.target_page == "docs/a.html"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedLink {
    /// Index into ParsedFile::links.
    pub link_index: usize,
    /// Canonical project-relative path of the stored page the link names.
    pub target_page: String,
}

/// Resolves the links of one page that name a stored page. `present` answers whether a
/// project file exists in the acknowledged inventory, so a link to a page added earlier
/// names that page.
pub(crate) fn resolve_file(
    file: &ScannedFile,
    present: impl Fn(&str) -> bool,
) -> Vec<ResolvedLink> {
    file.parsed
        .links
        .iter()
        .enumerate()
        .filter_map(|(link_index, link)| {
            let target_page = link_candidates(
                &file.entry.path,
                file.parsed.source_url.as_deref(),
                &link.href,
            )
            .into_iter()
            .find(|page| present(page))?;
            Some(ResolvedLink {
                link_index,
                target_page,
            })
        })
        .collect()
}

/// Every stored page a file's same-site links may name, present or not. Reference repair
/// indexes these so a removed page re-projects the pages that linked to it.
pub(crate) fn candidate_pages(file: &ScannedFile) -> BTreeSet<String> {
    file.parsed
        .links
        .iter()
        .flat_map(|link| {
            link_candidates(
                &file.entry.path,
                file.parsed.source_url.as_deref(),
                &link.href,
            )
        })
        .collect()
}

/// Project files a same-site href may name, in lookup order. Empty for links leaving the
/// site. Resolves `href` against the page's source URL, or against the page's project
/// path when no source URL is declared. Root-relative paths name the project root then.
fn link_candidates(page: &str, source_url: Option<&str>, href: &str) -> Vec<String> {
    let base = source_url.map(split);
    let page_path = format!("/{page}");
    let (base_scheme, base_authority, base_path) = match &base {
        Some(base) => (
            base.scheme,
            base.authority,
            if base.path.is_empty() { "/" } else { base.path },
        ),
        None => (None, None, page_path.as_str()),
    };
    let reference = split(href);
    let (scheme, authority, path) = if reference.scheme.is_some() {
        (
            reference.scheme,
            reference.authority,
            reference.path.to_owned(),
        )
    } else if reference.authority.is_some() {
        (base_scheme, reference.authority, reference.path.to_owned())
    } else if reference.path.starts_with('/') {
        (base_scheme, base_authority, reference.path.to_owned())
    } else if reference.path.is_empty() {
        (base_scheme, base_authority, base_path.to_owned())
    } else {
        let directory = &base_path[..=base_path.rfind('/').unwrap_or(0)];
        (
            base_scheme,
            base_authority,
            format!("{directory}{}", reference.path),
        )
    };
    if !(same(scheme, base_scheme) && same(authority, base_authority)) {
        return Vec::new();
    }
    page_candidates(&remove_dot_segments(&path))
}

/// Project files a site path may name, in lookup order: an `.html`/`.htm` path is the
/// file itself, a trailing slash names `index.html`, and any other path tries
/// `<path>.html` and then `<path>/index.html`.
fn page_candidates(path: &str) -> Vec<String> {
    let Some(decoded) = percent_decode(path.trim_start_matches('/')) else {
        return Vec::new();
    };
    let lower = decoded.to_ascii_lowercase();
    let candidates = if lower.ends_with(".html") || lower.ends_with(".htm") {
        vec![decoded]
    } else if decoded.is_empty() || decoded.ends_with('/') {
        vec![format!("{decoded}index.html")]
    } else {
        vec![format!("{decoded}.html"), format!("{decoded}/index.html")]
    };
    candidates
        .into_iter()
        .filter(|candidate| validate_path(candidate).is_ok())
        .collect()
}

/// A URI reference split into scheme, authority and path; any `?query` or `#fragment` is dropped.
struct Parts<'a> {
    scheme: Option<&'a str>,
    authority: Option<&'a str>,
    path: &'a str,
}

fn split(reference: &str) -> Parts<'_> {
    let (scheme, rest) = match reference.split_once(':') {
        Some((scheme, rest)) if is_scheme(scheme) => (Some(scheme), rest),
        _ => (None, reference),
    };
    let (authority, rest) = match rest.strip_prefix("//") {
        Some(after) => {
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            (Some(&after[..end]), &after[end..])
        }
        None => (None, rest),
    };
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    Parts {
        scheme,
        authority,
        path: &rest[..end],
    }
}

fn is_scheme(text: &str) -> bool {
    text.starts_with(|character: char| character.is_ascii_alphabetic())
        && text.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

/// Compares optional URI components; two missing components match each other.
fn same(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        _ => false,
    }
}

/// RFC 3986 dot-segment removal for an absolute path. Never climbs above the root.
fn remove_dot_segments(path: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for segment in path.split('/').skip(1) {
        match segment {
            "." => {}
            ".." => {
                kept.pop();
            }
            _ => kept.push(segment),
        }
    }
    if path.ends_with("/.") || path.ends_with("/..") {
        kept.push("");
    }
    format!("/{}", kept.join("/"))
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}
