//! Turns an HTML syntax tree into heading sections, site chrome and hyperlinks.
//! Visible text and link targets come only from the page itself: script, style and
//! embedded graphics never contribute words or links.

use crate::{DocumentLink, IndexedDefinition, ParsedFile, SourceFingerprint, SourceSpan};
use tree_sitter::{Node, Tree};

/// Subtrees with neither visible text nor page links.
const SKIPPED: &[&str] = &["script", "style", "svg", "noscript", "template"];
/// Site chrome stays in the graph but is marked, so navigation is not mistaken for content.
const CHROME: &[&str] = &["nav", "header", "footer", "aside"];
/// Tags whose boundaries separate words even when the source has no whitespace.
const BLOCK: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "br",
    "caption",
    "dd",
    "details",
    "dialog",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
];

/// Element nesting beyond this depth is not walked, bounding recursion; browsers cap DOM depth too.
// shortcut: content nested deeper than 512 levels is not indexed, raise it when a real page needs more.
const MAX_DEPTH: usize = 512;

pub(super) fn extract(tree: &Tree, source: &str) -> ParsedFile {
    let mut walk = Walk::new(source);
    walk.children(tree.root_node());
    walk.finish()
}

/// Who receives visible text and links: the innermost open chrome element, else the
/// innermost open heading section, else the file element.
#[derive(Clone, Copy)]
enum Owner {
    File,
    Section(usize),
    Chrome(usize),
}

struct Section {
    start: usize,
    end: usize,
    level: usize,
    name: String,
    text: String,
}

struct Chrome {
    start: usize,
    end: usize,
    name: String,
}

struct PendingLink {
    href: String,
    label: String,
    owner: Owner,
    line: usize,
}

struct Walk<'a> {
    source: &'a str,
    line_starts: Vec<usize>,
    sections: Vec<Section>,
    chrome: Vec<Chrome>,
    open_sections: Vec<usize>,
    open_chrome: Vec<usize>,
    links: Vec<PendingLink>,
    head_depth: usize,
    depth: usize,
    text: TextCursor,
    /// Receives text of non-section owners so they still advance the text cursor.
    scratch: String,
    source_url: Option<String>,
}

impl<'a> Walk<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            line_starts: std::iter::once(0)
                .chain(source.match_indices('\n').map(|(index, _)| index + 1))
                .collect(),
            sections: Vec::new(),
            chrome: Vec::new(),
            open_sections: Vec::new(),
            open_chrome: Vec::new(),
            links: Vec::new(),
            head_depth: 0,
            depth: 0,
            text: TextCursor { last_end: 0 },
            scratch: String::new(),
            source_url: None,
        }
    }

    fn children(&mut self, node: Node<'_>) {
        if self.depth == MAX_DEPTH {
            return;
        }
        self.depth += 1;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(child);
        }
        self.depth -= 1;
    }

    fn visit(&mut self, node: Node<'_>) {
        match node.kind() {
            "text" | "entity" => self.leaf(node),
            _ if stray_text(node, self.source) => self.leaf(node),
            "script_element" | "style_element" | "start_tag" | "end_tag" => {}
            "element" | "self_closing_tag" => self.element(node),
            _ => self.children(node),
        }
    }

    fn element(&mut self, node: Node<'_>) {
        let tag = element_tag(node, self.source);
        if SKIPPED.contains(&tag.as_str()) {
            return;
        }
        let block = BLOCK.contains(&tag.as_str());
        if block {
            self.text_out().push(' ');
        }
        if let Some(level) = heading_level(&tag).filter(|_| self.open_chrome.is_empty()) {
            self.heading(node, level, &tag);
        } else if CHROME.contains(&tag.as_str()) {
            self.chrome_element(node, &tag);
        } else if tag == "a" {
            self.anchor(node);
        } else if tag == "meta" {
            self.meta(node);
            self.children(node);
        } else if tag == "head" {
            self.head_depth += 1;
            self.children(node);
            self.head_depth -= 1;
        } else {
            self.children(node);
        }
        if block {
            self.text_out().push(' ');
        }
    }

    fn heading(&mut self, node: Node<'_>, level: usize, tag: &str) {
        let start = node.start_byte();
        // A heading ends every open section of its own or a deeper level.
        while let Some(&open) = self.open_sections.last() {
            if self.sections[open].level < level {
                break;
            }
            self.sections[open].end = start;
            self.open_sections.pop();
        }
        let name = visible_text(node, self.source);
        self.sections.push(Section {
            start,
            end: self.source.len(),
            level,
            name: if name.is_empty() {
                tag.to_owned()
            } else {
                name
            },
            text: String::new(),
        });
        self.open_sections.push(self.sections.len() - 1);
        self.children(node);
    }

    fn chrome_element(&mut self, node: Node<'_>, tag: &str) {
        self.chrome.push(Chrome {
            start: node.start_byte(),
            end: node.end_byte(),
            name: tag.to_owned(),
        });
        self.open_chrome.push(self.chrome.len() - 1);
        self.children(node);
        self.open_chrome.pop();
    }

    fn anchor(&mut self, node: Node<'_>) {
        if let Some(href) = start_tag(node).and_then(|tag| attribute(tag, self.source, "href")) {
            let href = href.trim();
            if is_page_href(href) {
                self.links.push(PendingLink {
                    href: href.to_owned(),
                    label: visible_text(node, self.source),
                    owner: self.owner(),
                    line: node.start_position().row + 1,
                });
            }
        }
        self.children(node);
    }

    fn meta(&mut self, node: Node<'_>) {
        if self.head_depth == 0 {
            return;
        }
        let Some(tag) = start_tag(node) else {
            return;
        };
        let (Some(name), Some(content)) = (
            attribute(tag, self.source, "name"),
            attribute(tag, self.source, "content"),
        ) else {
            return;
        };
        match name.as_str() {
            "lumvise:source-url" if self.source_url.is_none() => {
                let url = content.trim();
                if !url.is_empty() {
                    self.source_url = Some(url.to_owned());
                }
            }
            _ => {}
        }
    }

    fn leaf(&mut self, node: Node<'_>) {
        let out = match self.owner() {
            Owner::Section(index) => &mut self.sections[index].text,
            _ => {
                self.scratch.clear();
                &mut self.scratch
            }
        };
        self.text.leaf(self.source, node, out);
    }

    fn text_out(&mut self) -> &mut String {
        match self.owner() {
            Owner::Section(index) => &mut self.sections[index].text,
            _ => {
                self.scratch.clear();
                &mut self.scratch
            }
        }
    }

    fn owner(&self) -> Owner {
        match (self.open_chrome.last(), self.open_sections.last()) {
            (Some(&chrome), _) => Owner::Chrome(chrome),
            (None, Some(&section)) => Owner::Section(section),
            (None, None) => Owner::File,
        }
    }

    fn line_of(&self, byte: usize) -> usize {
        self.line_starts.partition_point(|start| *start <= byte)
    }

    fn definition(
        &self,
        kind: &str,
        name: &str,
        start: usize,
        end: usize,
        html_fingerprint: Option<SourceFingerprint>,
    ) -> IndexedDefinition {
        IndexedDefinition {
            kind: kind.into(),
            name: name.into(),
            start_line: self.line_of(start),
            end_line: self.line_of(end.saturating_sub(1).max(start)),
            span: SourceSpan { start, end },
            implementation_type: None,
            html_fingerprint,
        }
    }

    fn finish(self) -> ParsedFile {
        let section_count = self.sections.len();
        // A section's identity is its visible words, then its own link targets in order.
        let mut targets: Vec<Vec<&str>> = vec![Vec::new(); section_count];
        for link in &self.links {
            if let Owner::Section(index) = link.owner {
                targets[index].push(link.href.as_str());
            }
        }
        let mut definitions: Vec<IndexedDefinition> = self
            .sections
            .iter()
            .zip(&targets)
            .map(|(section, targets)| {
                let words = collapse(&decode_entities(&section.text));
                let identity = std::iter::once(words.as_str())
                    .chain(targets.iter().copied())
                    .collect::<Vec<_>>()
                    .join("\n");
                self.definition(
                    "html_section",
                    &section.name,
                    section.start,
                    section.end,
                    Some(SourceFingerprint::text(&identity)),
                )
            })
            .collect();
        definitions.extend(self.chrome.iter().map(|chrome| {
            self.definition("html_chrome", &chrome.name, chrome.start, chrome.end, None)
        }));
        let links = self
            .links
            .iter()
            .map(|link| DocumentLink {
                href: link.href.clone(),
                label: link.label.clone(),
                owner_definition: match link.owner {
                    Owner::File => None,
                    Owner::Section(index) => Some(index),
                    Owner::Chrome(index) => Some(section_count + index),
                },
                line: link.line,
            })
            .collect();
        ParsedFile {
            definitions,
            links,
            source_url: self.source_url,
            ..ParsedFile::default()
        }
    }
}

/// Appends visible text in document order. Source whitespace outside markup or a
/// block boundary separates words; inline tags do not, so `Tom<a>x</a>` stays `Tomx`.
struct TextCursor {
    last_end: usize,
}

impl TextCursor {
    fn leaf(&mut self, source: &str, node: Node<'_>, out: &mut String) {
        let (start, end) = (node.start_byte(), node.end_byte());
        if start > self.last_end && separates(&source[self.last_end..start]) {
            out.push(' ');
        }
        out.push_str(&source[start..end]);
        self.last_end = end;
    }
}

/// Whether whitespace outside any tag separates two text leaves.
fn separates(gap: &str) -> bool {
    let mut in_tag = false;
    gap.chars().any(|character| match character {
        '<' => {
            in_tag = true;
            false
        }
        '>' => {
            in_tag = false;
            false
        }
        _ => !in_tag && character.is_whitespace(),
    })
}

/// Error nodes cover characters no rule accepts, such as a bare `&` or `>`, and can
/// swallow the words after them, so their children are not text. HTML renders those
/// characters as text, so the raw bytes keep their place; a `<` makes the node a broken
/// tag fragment, which is markup and is walked instead.
fn stray_text(node: Node<'_>, source: &str) -> bool {
    node.is_error() && !source[node.byte_range()].contains('<')
}

/// Visible text of one element subtree, whitespace-collapsed and entity-decoded.
fn visible_text(node: Node<'_>, source: &str) -> String {
    let mut text = String::new();
    let mut cursor = TextCursor {
        last_end: node.start_byte(),
    };
    collect(node, source, &mut cursor, &mut text, 0);
    collapse(&decode_entities(&text))
}

fn collect(node: Node<'_>, source: &str, cursor: &mut TextCursor, out: &mut String, depth: usize) {
    match node.kind() {
        "text" | "entity" => cursor.leaf(source, node, out),
        _ if stray_text(node, source) => cursor.leaf(source, node, out),
        "script_element" | "style_element" | "start_tag" | "end_tag" => {}
        _ if depth == MAX_DEPTH => {}
        _ => {
            let tag = element_tag(node, source);
            if SKIPPED.contains(&tag.as_str()) {
                return;
            }
            let block = BLOCK.contains(&tag.as_str());
            if block {
                out.push(' ');
            }
            let mut children = node.walk();
            for child in node.named_children(&mut children) {
                collect(child, source, cursor, out, depth + 1);
            }
            if block {
                out.push(' ');
            }
        }
    }
}

fn start_tag(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() == "self_closing_tag" {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == "start_tag")
}

fn element_tag(node: Node<'_>, source: &str) -> String {
    start_tag(node).map_or_else(String::new, |tag| tag_name(tag, source))
}

fn tag_name(start: Node<'_>, source: &str) -> String {
    let mut cursor = start.walk();
    let name = start
        .named_children(&mut cursor)
        .find(|child| child.kind() == "tag_name");
    name.map_or_else(String::new, |child| {
        source[child.byte_range()].to_ascii_lowercase()
    })
}

/// Decoded value of the first attribute named `wanted`, if present.
fn attribute(tag: Node<'_>, source: &str, wanted: &str) -> Option<String> {
    let mut cursor = tag.walk();
    for attribute in tag.named_children(&mut cursor) {
        if attribute.kind() != "attribute" {
            continue;
        }
        let mut inner = attribute.walk();
        let mut parts = attribute.named_children(&mut inner);
        let Some(name) = parts.next() else {
            continue;
        };
        if !source[name.byte_range()].eq_ignore_ascii_case(wanted) {
            continue;
        }
        let value = parts
            .next()
            .map_or_else(String::new, |value| attribute_text(value, source));
        return Some(decode_entities(&value));
    }
    None
}

fn attribute_text(value: Node<'_>, source: &str) -> String {
    let raw = &source[value.byte_range()];
    if value.kind() == "quoted_attribute_value" {
        raw.get(1..raw.len().saturating_sub(1))
            .unwrap_or_default()
            .to_owned()
    } else {
        raw.to_owned()
    }
}

/// Keeps hrefs that name a destination; fragments and non-navigational schemes never
/// become page relationships.
fn is_page_href(href: &str) -> bool {
    let lower = href.to_ascii_lowercase();
    !href.is_empty()
        && !href.starts_with('#')
        && !["javascript:", "mailto:", "tel:"]
            .iter()
            .any(|scheme| lower.starts_with(scheme))
}

fn heading_level(tag: &str) -> Option<usize> {
    match tag {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(text: &str) -> String {
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        decoded.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        match decode_reference(tail) {
            Some((character, length)) => {
                decoded.push(character);
                rest = &tail[length..];
            }
            None => {
                decoded.push('&');
                rest = &tail[1..];
            }
        }
    }
    decoded.push_str(rest);
    decoded
}

/// Decodes one leading `&name;` or `&#n;` reference; the length covers all of it.
fn decode_reference(tail: &str) -> Option<(char, usize)> {
    // The longest HTML named reference is 29 characters; bound the search window.
    let limit = tail
        .char_indices()
        .nth(34)
        .map_or(tail.len(), |(index, _)| index);
    let end = tail[..limit].find(';')?;
    let name = &tail[1..end];
    let character = match name.strip_prefix('#') {
        Some(hex) if hex.starts_with(['x', 'X']) => {
            char::from_u32(u32::from_str_radix(&hex[1..], 16).ok()?)?
        }
        Some(decimal) => char::from_u32(decimal.parse().ok()?)?,
        None => named_entity(name)?,
    };
    Some((character, end + 1))
}

// shortcut: only common named references decode; other names stay literal, add them when a page needs them.
fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "ndash" => '\u{2013}',
        "mdash" => '\u{2014}',
        "hellip" => '\u{2026}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "copy" => '\u{a9}',
        "reg" => '\u{ae}',
        "trade" => '\u{2122}',
        "times" => '\u{d7}',
        "middot" => '\u{b7}',
        "bull" => '\u{2022}',
        "deg" => '\u{b0}',
        "euro" => '\u{20ac}',
        "laquo" => '\u{ab}',
        "raquo" => '\u{bb}',
        _ => return None,
    })
}
