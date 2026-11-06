//! Resolves HTML hyperlinks against the acknowledged project inventory. A link is
//! either a project page, identified by path, or a locator string; links never become
//! name references.

use crate::ScannedFile;
use crate::source::validate_path;
use std::collections::BTreeSet;

/// Where one page link points after resolution. Example: `LinkTarget::Page("docs/a.html".into())`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// Project page by canonical project-relative path; projection maps it to a file element.
    Page(String),
    /// External URL, or the project-relative location when the page declares no source URL.
    Locator(String),
}

/// Resolution of one parsed link. Example: `file.parsed.links[resolved.link_index]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedLink {
    /// Index into ParsedFile::links.
    pub link_index: usize,
    /// Target as seen by the inventory that resolved it.
    pub target: LinkTarget,
}

/// Resolves every link of one page. `present` answers whether a project file exists in
/// the acknowledged inventory, so a link to a page added earlier names that page.
pub(crate) fn resolve_file(
    file: &ScannedFile,
    present: impl Fn(&str) -> bool,
) -> Vec<ResolvedLink> {
    file.parsed
        .links
        .iter()
        .enumerate()
        .map(|(link_index, link)| {
            let classified = classify(
                &file.entry.path,
                file.parsed.source_url.as_deref(),
                &link.href,
            );
            let target = match classified.pages.into_iter().find(|page| present(page)) {
                Some(page) => LinkTarget::Page(page),
                None => LinkTarget::Locator(classified.locator),
            };
            ResolvedLink { link_index, target }
        })
        .collect()
}

/// Every project page a file's same-site links may name, present or not. Reference
/// repair indexes these so a page appearing or vanishing re-resolves its linkers.
pub(crate) fn candidate_pages(file: &ScannedFile) -> BTreeSet<String> {
    file.parsed
        .links
        .iter()
        .flat_map(|link| {
            classify(
                &file.entry.path,
                file.parsed.source_url.as_deref(),
                &link.href,
            )
            .pages
        })
        .collect()
}

struct Classified {
    /// Candidate project files in lookup order; empty for links leaving the site.
    pages: Vec<String>,
    /// Absolute URL, or the project-relative path when there is no source URL.
    locator: String,
}

/// Resolves `href` against the page's source URL, or against the page's project path
/// when no source URL is declared. Root-relative paths name the project root then.
fn classify(page: &str, source_url: Option<&str>, href: &str) -> Classified {
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
        let locator = match (reference.authority, base_scheme) {
            // A network-path reference such as `//host/x` takes the page's scheme.
            (Some(_), Some(scheme)) if reference.scheme.is_none() => {
                format!("{scheme}:{}", href.trim())
            }
            _ => href.trim().to_owned(),
        };
        return Classified {
            pages: Vec::new(),
            locator,
        };
    }
    let path = remove_dot_segments(&path);
    let locator = match (base_scheme, base_authority) {
        (Some(scheme), Some(authority)) => {
            format!("{scheme}://{authority}{path}{}", reference.suffix)
        }
        _ => format!("{}{}", path.trim_start_matches('/'), reference.suffix),
    };
    Classified {
        pages: page_candidates(&path),
        locator,
    }
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

/// A URI reference split into its parts; `suffix` keeps the `?query` and `#fragment`.
struct Parts<'a> {
    scheme: Option<&'a str>,
    authority: Option<&'a str>,
    path: &'a str,
    suffix: &'a str,
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
        suffix: &rest[end..],
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
