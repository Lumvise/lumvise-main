//! Logical rows only; DB Core owns transactions, SQL, resource encoding and authorization.
use super::*;
use lumvise_db_core::{
    DbError, PluginDataExpectedValue, PluginDataMutation, RelationalOperation,
    RelationalPersistence, RelationalResult,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use sha2::{Digest, Sha256};

const OWNER: &str = "app.index-publication";
const TABLE: &str = "source-publications";

pub(super) struct CheckpointRows {
    relational: Arc<dyn RelationalPersistence>,
    prefix: String,
}

impl CheckpointRows {
    pub(super) fn producer_selection_id(&self) -> String {
        format!("producer:{}", self.prefix.trim_end_matches('/'))
    }

    pub(super) fn new(
        relational: Arc<dyn RelationalPersistence>,
        binding: &IndexCheckoutBinding,
    ) -> Result<Self> {
        Self::namespace(relational, binding)
    }

    pub(super) fn source_route(
        relational: Arc<dyn RelationalPersistence>,
        binding: &IndexCheckoutBinding,
    ) -> Result<Self> {
        Self::namespace(
            relational,
            &(
                "logical-source-route-1",
                &binding.client_instance_id,
                &binding.checkout_id,
                &binding.project_id,
                &binding.provider_instance_id,
                &binding.selection_id,
                &binding.source_context,
            ),
        )
    }

    fn namespace(
        relational: Arc<dyn RelationalPersistence>,
        identity: &impl Serialize,
    ) -> Result<Self> {
        let bytes = serde_json::to_vec(identity).map_err(DbError::from)?;
        Ok(Self {
            relational,
            prefix: format!("{:x}/", Sha256::digest(bytes)),
        })
    }

    pub(super) fn ensure(&self, control: &InvocationControl) -> Result<()> {
        self.execute(
            RelationalOperation::EnsurePluginDataTable {
                plugin_id: OWNER.into(),
                table_name: TABLE.into(),
                schema: json!({"owner":"source-publication-checkpoint"}),
            },
            control,
        )?;
        Ok(())
    }

    pub(super) fn get<T: DeserializeOwned>(
        &self,
        suffix: &str,
        control: &InvocationControl,
    ) -> Result<Option<T>> {
        let result = self.execute(
            RelationalOperation::GetPluginData {
                plugin_id: OWNER.into(),
                table_name: TABLE.into(),
                row_key: self.key(suffix),
            },
            control,
        )?;
        let RelationalResult::PluginDataRow(row) = result else {
            return Err(invalid_checkpoint(suffix, "a portable plugin row result"));
        };
        row.map(|row| {
            serde_json::from_value(row.value)
                .map_err(|_| invalid_checkpoint(suffix, std::any::type_name::<T>()))
        })
        .transpose()
    }

    pub(super) fn scan<T: DeserializeOwned>(
        &self,
        suffix: &str,
        control: &InvocationControl,
    ) -> Result<Vec<T>> {
        let mut after_key = None;
        let mut rows = Vec::new();
        loop {
            let result = self.execute(
                RelationalOperation::PagePluginData {
                    plugin_id: OWNER.into(),
                    table_name: TABLE.into(),
                    key_prefix: Some(self.key(suffix)),
                    after_key,
                    limit: 1000,
                },
                control,
            )?;
            let RelationalResult::PluginDataPage(page) = result else {
                return Err(invalid_checkpoint(suffix, "a portable plugin row page"));
            };
            for row in page.rows {
                rows.push(
                    serde_json::from_value(row.value)
                        .map_err(|_| invalid_checkpoint(row.row_key, std::any::type_name::<T>()))?,
                );
            }
            after_key = page.next_after_key;
            if after_key.is_none() {
                return Ok(rows);
            }
        }
    }

    pub(super) fn put(&self, suffix: &str, value: &impl Serialize) -> Result<PluginDataMutation> {
        Ok(PluginDataMutation::Put {
            table_name: TABLE.into(),
            row_key: self.key(suffix),
            value: serde_json::to_value(value).map_err(DbError::from)?,
        })
    }

    pub(super) fn delete(&self, suffix: &str) -> PluginDataMutation {
        PluginDataMutation::Delete {
            table_name: TABLE.into(),
            row_key: self.key(suffix),
        }
    }

    pub(super) fn require(&self, header: Option<&CheckpointHeader>) -> Result<PluginDataMutation> {
        self.require_value("header", header)
    }

    pub(super) fn require_value<T: Serialize>(
        &self,
        suffix: &str,
        value: Option<&T>,
    ) -> Result<PluginDataMutation> {
        let expected = value
            .map(serde_json::to_value)
            .transpose()
            .map_err(DbError::from)?
            .map_or(
                PluginDataExpectedValue::Absent,
                PluginDataExpectedValue::Present,
            );
        Ok(PluginDataMutation::RequireValue {
            table_name: TABLE.into(),
            row_key: self.key(suffix),
            expected,
        })
    }

    pub(super) fn apply(
        &self,
        mutations: Vec<PluginDataMutation>,
        control: &InvocationControl,
    ) -> Result<()> {
        let result = self.execute(
            RelationalOperation::ApplyMutations {
                plugin_id: OWNER.into(),
                mutations,
            },
            control,
        )?;
        if !matches!(result, RelationalResult::PluginDataMutations(_)) {
            return Err(invalid_checkpoint(
                TABLE,
                "an atomic plugin row mutation result",
            ));
        }
        Ok(())
    }

    fn execute(
        &self,
        operation: RelationalOperation,
        control: &InvocationControl,
    ) -> Result<RelationalResult> {
        Ok(self.relational.execute(operation, control)?)
    }

    fn key(&self, suffix: &str) -> String {
        format!("{}{suffix}", self.prefix)
    }
}

pub(super) fn path_key(path: &str) -> String {
    format!("path/{path}")
}
pub(super) fn parser_key(id: &str) -> String {
    format!("parser/{:x}", Sha256::digest(id.as_bytes()))
}
pub(super) fn pending_key(digest: &str, suffix: &str) -> String {
    format!("pending/{digest}/{suffix}")
}
