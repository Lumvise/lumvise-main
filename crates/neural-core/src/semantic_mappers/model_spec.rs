use crate::error::{NeuralError, Result};
use crate::semantic_mappers::text_profile::{TextProfile, lexical_score, normalize_alias};
use crate::semantic_mappers::{SemanticMappingCandidate, SemanticMappingResult};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ModelSpecRanker {
    model_id: String,
    model_dir: PathBuf,
    aliases: HashMap<String, HashSet<String>>,
    boosts: HashMap<String, u32>,
}

#[derive(Debug, Deserialize)]
struct SemanticModelSpec {
    model_id: Option<String>,
    aliases: Option<BTreeMap<String, Vec<String>>>,
    boosts: Option<BTreeMap<String, u32>>,
}

impl ModelSpecRanker {
    /// Loads an ArchRouter semantic model spec from a model directory.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let ranker = ModelSpecRanker::from_dir(model_dir)?;
    /// ```
    pub fn from_dir(model_dir: PathBuf) -> Result<Self> {
        let spec = load_model_spec(&model_dir)?;
        Ok(Self::new(model_dir, spec))
    }

    /// Ranks candidates with the loaded ArchRouter aliases and boosts.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let results = ranker.rank("policy", &candidates);
    /// ```
    pub fn rank(
        &self,
        source: &str,
        candidates: &[SemanticMappingCandidate],
    ) -> Vec<SemanticMappingResult> {
        let query = TextProfile::new(source);
        let mut results: Vec<_> = candidates
            .iter()
            .map(|candidate| self.result(candidate, &query))
            .collect();
        results.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.candidate_id.cmp(&right.candidate_id))
        });
        results
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    fn new(model_dir: PathBuf, spec: SemanticModelSpec) -> Self {
        Self {
            model_id: model_id(spec.model_id),
            model_dir,
            aliases: aliases(spec.aliases),
            boosts: boosts(spec.boosts),
        }
    }

    fn result(
        &self,
        candidate: &SemanticMappingCandidate,
        query: &TextProfile,
    ) -> SemanticMappingResult {
        let score = self.score(query, &candidate.content);
        SemanticMappingResult {
            candidate_id: candidate.candidate_id.clone(),
            score: score as f32,
            explanation: format!("{} model-spec semantic route score", self.model_id),
            metadata: json!({
                "mode": "ArchRouter",
                "model_id": self.model_id,
                "model_dir": self.model_dir,
            }),
        }
    }

    fn score(&self, query: &TextProfile, text: &str) -> u32 {
        let candidate = TextProfile::new(text);
        let mut score = lexical_score(query, text);
        for term in &query.unique_terms {
            score += self.alias_score(term, &candidate);
            score += self.boost_score(term);
        }
        score
    }

    fn alias_score(&self, term: &str, candidate: &TextProfile) -> u32 {
        self.expand_term(term)
            .iter()
            .filter(|expanded| candidate.unique_terms.contains(*expanded))
            .count() as u32
            * 1_800
    }

    fn boost_score(&self, term: &str) -> u32 {
        self.expand_term(term)
            .iter()
            .filter_map(|expanded| self.boosts.get(expanded))
            .sum()
    }

    fn expand_term(&self, term: &str) -> HashSet<String> {
        let mut expanded = HashSet::from([term.to_string()]);
        if let Some(aliases) = self.aliases.get(term) {
            expanded.extend(aliases.iter().cloned());
        }
        expanded
    }
}

fn load_model_spec(model_dir: &Path) -> Result<SemanticModelSpec> {
    let path = spec_path(model_dir)?;
    let json = std::fs::read_to_string(&path).map_err(|source| NeuralError::Io {
        value: path.display().to_string(),
        expected: "readable ArchRouter model spec".to_string(),
        source,
    })?;
    serde_json::from_str(&json).map_err(|source| NeuralError::Json {
        value: path.display().to_string(),
        expected: "valid ArchRouter model spec JSON".to_string(),
        source,
    })
}

fn spec_path(model_dir: &Path) -> Result<PathBuf> {
    ["semantic-router.json", "router.json"]
        .iter()
        .map(|file_name| model_dir.join(file_name))
        .find(|path| path.is_file())
        .ok_or_else(|| NeuralError::MissingValue {
            value: model_dir.display().to_string(),
            expected: "semantic-router.json or router.json".to_string(),
        })
}

fn aliases(input: Option<BTreeMap<String, Vec<String>>>) -> HashMap<String, HashSet<String>> {
    let mut aliases = HashMap::<String, HashSet<String>>::new();
    for (key, values) in input.unwrap_or_default() {
        insert_aliases(&mut aliases, key, values);
    }
    aliases
}

fn insert_aliases(
    aliases: &mut HashMap<String, HashSet<String>>,
    key: String,
    values: Vec<String>,
) {
    let key = normalize_alias(&key);
    if key.is_empty() {
        return;
    }
    for value in values {
        insert_alias_pair(aliases, &key, &normalize_alias(&value));
    }
}

fn insert_alias_pair(aliases: &mut HashMap<String, HashSet<String>>, left: &str, right: &str) {
    if right.is_empty() {
        return;
    }
    aliases
        .entry(left.to_string())
        .or_default()
        .insert(right.to_string());
    aliases
        .entry(right.to_string())
        .or_default()
        .insert(left.to_string());
}

fn boosts(input: Option<BTreeMap<String, u32>>) -> HashMap<String, u32> {
    input
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(term, boost)| {
            let term = normalize_alias(&term);
            (!term.is_empty()).then_some((term, boost))
        })
        .collect()
}

fn model_id(input: Option<String>) -> String {
    input
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "semantic-router-spec".to_string())
}
