use std::collections::HashMap;

use jsonschema::Validator;
use lumvise_plugin_package::{ExportDescriptor, InstalledPlugin};
use lumvise_plugin_protocol::WireOutcome;
use serde_json::Value;

use crate::{PluginRuntimeError, SchemaDirection};

pub(crate) struct ExportValidators {
    input: Validator,
    input_schema: Value,
    output: Validator,
    output_schema: Value,
}

pub(crate) fn compile_exports(
    package: &InstalledPlugin,
) -> Result<HashMap<String, ExportValidators>, PluginRuntimeError> {
    package
        .exports()
        .iter()
        .map(|export| compile_export(package.plugin_id(), export))
        .collect()
}

fn compile_export(
    plugin_id: &str,
    export: &ExportDescriptor,
) -> Result<(String, ExportValidators), PluginRuntimeError> {
    let input = compile_schema(
        plugin_id,
        &export.id,
        SchemaDirection::Input,
        &export.input_schema,
    )?;
    let output = compile_schema(
        plugin_id,
        &export.id,
        SchemaDirection::Output,
        &export.output_schema,
    )?;
    Ok((
        export.id.clone(),
        ExportValidators {
            input,
            input_schema: export.input_schema.clone(),
            output,
            output_schema: export.output_schema.clone(),
        },
    ))
}

fn compile_schema(
    plugin_id: &str,
    export_id: &str,
    direction: SchemaDirection,
    schema: &Value,
) -> Result<Validator, PluginRuntimeError> {
    jsonschema::draft202012::new(schema).map_err(|error| PluginRuntimeError::SchemaCompilation {
        plugin_id: plugin_id.to_owned(),
        export_id: export_id.to_owned(),
        direction,
        message: error.to_string(),
    })
}

impl ExportValidators {
    pub(crate) fn validate_input(
        &self,
        plugin_id: &str,
        export_id: &str,
        input: &Value,
    ) -> Result<(), PluginRuntimeError> {
        validate_instance(
            &self.input,
            plugin_id,
            export_id,
            SchemaDirection::Input,
            input,
            &self.input_schema,
        )
    }

    pub(crate) fn validate_output(
        &self,
        plugin_id: &str,
        export_id: &str,
        outcome: &WireOutcome,
    ) -> Result<(), PluginRuntimeError> {
        let WireOutcome::Succeeded { value } = outcome else {
            return Ok(());
        };
        validate_instance(
            &self.output,
            plugin_id,
            export_id,
            SchemaDirection::Output,
            value,
            &self.output_schema,
        )
    }
}

fn validate_instance(
    validator: &Validator,
    plugin_id: &str,
    export_id: &str,
    direction: SchemaDirection,
    instance: &Value,
    schema: &Value,
) -> Result<(), PluginRuntimeError> {
    let Some(error) = validator.iter_errors(instance).next() else {
        return Ok(());
    };
    let schema_path = error.schema_path().as_str();
    let expected = schema.pointer(schema_path).unwrap_or(schema);
    let message = format!(
        "offending value {}; expected signed schema fragment {} at `{schema_path}`; validator: {error}",
        error.instance(),
        expected,
    );
    Err(PluginRuntimeError::SchemaValidation {
        plugin_id: plugin_id.to_owned(),
        export_id: export_id.to_owned(),
        direction,
        instance_path: error.instance_path().as_str().to_owned(),
        message,
    })
}
