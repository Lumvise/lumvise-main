use crate::error::{NeuralError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticMapperMode {
    Lexical,
    ArchRouter,
    AllMiniLmL6V2,
    BgeSmallEnV15,
    BgeM3,
    MxbaiEmbedXsmallV1,
}

impl SemanticMapperMode {
    /// Parses a mapper mode name from CLI/config text.
    ///
    /// # Example
    ///
    /// ```
    /// let mode = lumvise_neural_core::semantic_mappers::SemanticMapperMode::parse("Lexical").unwrap();
    /// assert_eq!(mode, lumvise_neural_core::semantic_mappers::SemanticMapperMode::Lexical);
    /// ```
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "Lexical" | "lexical" => Ok(Self::Lexical),
            "ArchRouter" | "arch_router" => Ok(Self::ArchRouter),
            "AllMiniLmL6V2" | "all_mini_lm_l6_v2" => Ok(Self::AllMiniLmL6V2),
            "BgeSmallEnV15" | "bge_small_en_v15" => Ok(Self::BgeSmallEnV15),
            "BgeM3" | "bge_m3" => Ok(Self::BgeM3),
            "MxbaiEmbedXsmallV1" | "mxbai_embed_xsmall_v1" => Ok(Self::MxbaiEmbedXsmallV1),
            _ => Err(NeuralError::InvalidValue {
                value: value.to_string(),
                expected: "one of Lexical, ArchRouter, AllMiniLmL6V2, BgeSmallEnV15, BgeM3, MxbaiEmbedXsmallV1".to_string(),
            }),
        }
    }

    pub(crate) fn requires_embeddings(self) -> bool {
        !matches!(self, Self::Lexical)
    }
}
