use super::{
    DefinitionNames, DefinitionSite, ReferenceFileUpdate, ReferenceTarget, ResolvedReference,
};
use crate::{IndexedReference, ParseStatus, ReferenceQualifier, ScannedFile};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::sync::Arc;

pub(super) struct ResolutionView<'a> {
    acknowledged: &'a DefinitionNames,
    added: &'a DefinitionNames,
    touched: &'a BTreeSet<String>,
    candidates: BTreeMap<String, CandidateSet>,
}

impl<'a> ResolutionView<'a> {
    pub(super) fn new(
        acknowledged: &'a DefinitionNames,
        added: &'a DefinitionNames,
        touched: &'a BTreeSet<String>,
    ) -> Self {
        Self {
            acknowledged,
            added,
            touched,
            candidates: BTreeMap::new(),
        }
    }

    pub(super) fn resolve_file(&mut self, file: &Arc<ScannedFile>) -> ReferenceFileUpdate {
        let owners = reference_owners(file);
        let references = file
            .parsed
            .references
            .iter()
            .enumerate()
            .map(|(index, reference)| ResolvedReference {
                reference_index: index,
                owner_definition: owners[index],
                target: self.target(file, reference),
            })
            .collect();
        ReferenceFileUpdate {
            file: Arc::clone(file),
            references,
        }
    }

    fn target(&mut self, file: &ScannedFile, reference: &IndexedReference) -> ReferenceTarget {
        let key = reference.name.clone();
        let candidates = self.candidates.entry(key).or_insert_with(|| {
            let old = self
                .acknowledged
                .get(&reference.name)
                .into_iter()
                .flat_map(|sites| sites.values())
                .filter(|site| !self.touched.contains(&site.file.entry.path));
            let added = self
                .added
                .get(&reference.name)
                .into_iter()
                .flat_map(|sites| sites.values());
            CandidateSet::from_sites(old.chain(added))
        });
        let module = match &reference.qualifier {
            ReferenceQualifier::Scope(scope) => Some(
                scope
                    .strip_prefix("crate::")
                    .unwrap_or(scope)
                    .replace("::", "/"),
            ),
            _ => None,
        };
        let mut global = CandidateSummary::default();
        let mut local = CandidateSummary::default();
        let mut fallback_global = CandidateSummary::default();
        let mut fallback_local = CandidateSummary::default();
        for site in &candidates.sites {
            if compatible(file, reference, module.as_deref(), site) {
                global.add(site);
                if site.file.entry.path == file.entry.path {
                    local.add(site);
                }
            } else if fallback_compatible(file, reference, site) {
                fallback_global.add(site);
                if site.file.entry.path == file.entry.path {
                    fallback_local.add(site);
                }
            }
        }
        if local.count > 0 {
            local.target()
        } else if global.count > 0 {
            global.target()
        } else if fallback_local.count > 0 {
            fallback_local.target()
        } else {
            fallback_global.target()
        }
    }
}

#[derive(Default)]
struct CandidateSummary {
    count: usize,
    unique: Option<DefinitionSite>,
}

impl CandidateSummary {
    fn add(&mut self, site: &DefinitionSite) {
        self.count += 1;
        self.unique = (self.count == 1).then(|| site.clone());
    }

    fn target(&self) -> ReferenceTarget {
        match (&self.unique, self.count) {
            (Some(site), _) => ReferenceTarget::Unique(site.clone()),
            (_, 0) => ReferenceTarget::Unresolved,
            (_, count) => ReferenceTarget::Ambiguous {
                candidate_count: count,
            },
        }
    }
}

#[derive(Default)]
struct CandidateSet {
    sites: Vec<DefinitionSite>,
}

impl CandidateSet {
    fn from_sites<'a>(sites: impl Iterator<Item = &'a DefinitionSite>) -> Self {
        Self {
            sites: sites.cloned().collect(),
        }
    }
}

fn compatible(
    file: &ScannedFile,
    reference: &IndexedReference,
    module: Option<&str>,
    site: &DefinitionSite,
) -> bool {
    if !same_language(file, site) {
        return false;
    }
    let definition = &site.file.parsed.definitions[site.definition_index];
    match &reference.qualifier {
        ReferenceQualifier::Unqualified => definition.implementation_type.is_none(),
        ReferenceQualifier::UnqualifiedInType(enclosing_type) => {
            definition.implementation_type.is_none()
                || matches!(&file.parsed.status, ParseStatus::Parsed { language, .. } if matches!(language.as_str(), "csharp" | "cpp")
                    && definition.implementation_type.as_deref() == Some(enclosing_type))
        }
        ReferenceQualifier::Receiver(_) => false,
        ReferenceQualifier::Scope(scope) => {
            scoped_candidate(scope, module.expect("scope module"), site)
        }
        ReferenceQualifier::SelfScope(scope) => {
            definition.implementation_type.as_deref() == Some(scope)
        }
    }
}

fn fallback_compatible(
    file: &ScannedFile,
    reference: &IndexedReference,
    site: &DefinitionSite,
) -> bool {
    matches!(&reference.qualifier, ReferenceQualifier::SelfScope(_))
        && matches!(
            (&file.parsed.status, &site.file.parsed.status),
            (ParseStatus::Parsed { language: left, .. }, ParseStatus::Parsed { language: right, .. })
                if left == right
        )
        && site.file.parsed.definitions[site.definition_index]
            .implementation_type
            .is_some()
}

fn same_language(file: &ScannedFile, site: &DefinitionSite) -> bool {
    if let (
        ParseStatus::Parsed { language: left, .. },
        ParseStatus::Parsed {
            language: right, ..
        },
    ) = (&file.parsed.status, &site.file.parsed.status)
        && left != right
    {
        return false;
    }
    true
}

fn scoped_candidate(scope: &str, module: &str, site: &DefinitionSite) -> bool {
    let definition = &site.file.parsed.definitions[site.definition_index];
    if let Some(container) = &definition.implementation_type {
        return container == scope;
    }
    // Explicit module paths must agree with a file locator. Aliases and inline
    // modules remain unresolved until their scope can be established.
    let path = site.file.entry.path.trim_end_matches(".rs");
    path == module
        || path
            .strip_suffix(module)
            .is_some_and(|prefix| prefix.ends_with('/'))
        || path.strip_suffix("/mod").is_some_and(|prefix| {
            prefix == module
                || prefix
                    .strip_suffix(module)
                    .is_some_and(|root| root.ends_with('/'))
        })
}

fn reference_owners(file: &ScannedFile) -> Vec<Option<usize>> {
    let mut definitions: Vec<_> = file.parsed.definitions.iter().enumerate().collect();
    definitions.sort_unstable_by_key(|(_, definition)| definition.span.start);
    let mut references: Vec<_> = file.parsed.references.iter().enumerate().collect();
    references.sort_unstable_by_key(|(_, reference)| reference.span.start);
    let mut pending = definitions.into_iter().peekable();
    let mut active = BinaryHeap::new();
    let mut owners = vec![None; references.len()];
    for (index, reference) in references {
        while pending
            .peek()
            .is_some_and(|(_, definition)| definition.span.start <= reference.span.start)
        {
            let (definition_index, definition) = pending.next().expect("peeked definition");
            active.push((
                Reverse(definition.span.end.saturating_sub(definition.span.start)),
                Reverse(definition_index),
            ));
        }
        owners[index] = active_owner(&mut active, file, reference);
    }
    owners
}

fn active_owner(
    active: &mut BinaryHeap<(Reverse<usize>, Reverse<usize>)>,
    file: &ScannedFile,
    reference: &IndexedReference,
) -> Option<usize> {
    while let Some((_, Reverse(index))) = active.peek().copied() {
        if file.parsed.definitions[index].span.end >= reference.span.end {
            return Some(index);
        }
        active.pop();
    }
    None
}
