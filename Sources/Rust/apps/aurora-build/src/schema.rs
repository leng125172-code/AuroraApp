//! Versioned JSON Schema and deterministic collection validation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::digest::read_json;
use crate::error::{BuildError, BuildResult};

const SCHEMAS: [(&str, &str); 4] = [
    (
        "canonical-ir",
        "aurora/canonical-ir/v1/canonical-ir.schema.json",
    ),
    ("envelope", "aurora/envelope/v1/envelope.schema.json"),
    ("payload", "aurora/payload/v1/payload.schema.json"),
    (
        "target-profile",
        "aurora/target-profile/v1/target-profile.schema.json",
    ),
];

/// Validate schemas, positive samples, negative samples, and semantic invariants.
pub(crate) fn validate_all(repository_root: &Path) -> BuildResult<usize> {
    let schema_root = repository_root.join("Sources/Contracts/schema");
    let mut validators = BTreeMap::new();
    for (name, relative_path) in SCHEMAS {
        let path = schema_root.join(relative_path);
        let schema = read_json(&path)?;
        jsonschema::meta::validate(&schema).map_err(|error| {
            BuildError::Validation(format!(
                "schema `{}` does not conform to its declared meta-schema: {error}",
                path.display()
            ))
        })?;
        let validator = jsonschema::validator_for(&schema).map_err(|error| {
            BuildError::Validation(format!("compile schema `{}`: {error}", path.display()))
        })?;
        validators.insert(name, validator);
    }

    let examples_root = schema_root.join("examples");
    let mut examples = json_files(&examples_root)?;
    examples.sort();
    if examples.is_empty() {
        return Err(BuildError::Validation(format!(
            "no schema examples found in `{}`",
            examples_root.display()
        )));
    }

    for path in &examples {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                BuildError::Validation(format!("non-UTF-8 example path: {}", path.display()))
            })?;
        let schema_name = SCHEMAS
            .iter()
            .map(|(name, _path)| *name)
            .find(|name| file_name.starts_with(name))
            .ok_or_else(|| {
                BuildError::Validation(format!(
                    "example `{file_name}` does not map to a versioned schema"
                ))
            })?;
        let validator = validators.get(schema_name).ok_or_else(|| {
            BuildError::Validation(format!("schema validator missing for `{schema_name}`"))
        })?;
        let instance = read_json(path)?;
        let mut errors: Vec<String> = validator
            .iter_errors(&instance)
            .map(|error| error.to_string())
            .collect();
        if let Err(error) = validate_semantics(path, &instance) {
            errors.push(error.to_string());
        }
        let expects_failure = file_name.contains(".invalid-");
        match (expects_failure, errors.is_empty()) {
            (false, true) | (true, false) => {}
            (false, false) => {
                return Err(BuildError::Validation(format!(
                    "valid example `{}` failed `{schema_name}`: {}",
                    path.display(),
                    errors.join("; ")
                )));
            }
            (true, true) => {
                return Err(BuildError::Validation(format!(
                    "negative example `{}` unexpectedly passed `{schema_name}`",
                    path.display()
                )));
            }
        }
    }
    Ok(examples.len())
}

fn json_files(root: &Path) -> BuildResult<Vec<PathBuf>> {
    let entries = fs::read_dir(root).map_err(|source| BuildError::Io {
        operation: "list directory",
        path: root.to_path_buf(),
        source,
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| BuildError::Io {
            operation: "read directory entry",
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn validate_semantics(path: &Path, value: &Value) -> BuildResult<()> {
    validate_sorted_collections(path, value, "$")?;
    validate_u64_strings(path, value, "$")?;
    validate_versions(path, value, "$")
}

fn validate_sorted_collections(path: &Path, value: &Value, pointer: &str) -> BuildResult<()> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if key == "extensions" {
                    continue;
                }
                let child_pointer = format!("{pointer}/{key}");
                if matches!(
                    key.as_str(),
                    "capabilities" | "cpuFeatures" | "ioCapabilities"
                ) {
                    validate_sorted_strings(path, child, &child_pointer)?;
                } else if key == "artifacts" {
                    validate_sorted_object_key(path, child, &child_pointer, "path")?;
                } else if key == "contracts" {
                    validate_sorted_object_key(path, child, &child_pointer, "name")?;
                }
                validate_sorted_collections(path, child, &child_pointer)?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                validate_sorted_collections(path, child, &format!("{pointer}/{index}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_sorted_strings(path: &Path, value: &Value, pointer: &str) -> BuildResult<()> {
    let Some(items) = value.as_array() else {
        return Ok(());
    };
    let keys: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
    if keys.windows(2).all(|pair| pair[0] < pair[1]) {
        Ok(())
    } else {
        Err(BuildError::Validation(format!(
            "`{}` {pointer} must be strictly sorted and unique",
            path.display()
        )))
    }
}

fn validate_sorted_object_key(
    path: &Path,
    value: &Value,
    pointer: &str,
    key: &str,
) -> BuildResult<()> {
    let Some(items) = value.as_array() else {
        return Ok(());
    };
    let keys: Vec<&str> = items
        .iter()
        .filter_map(|item| item.get(key).and_then(Value::as_str))
        .collect();
    if keys.windows(2).all(|pair| pair[0] < pair[1]) {
        Ok(())
    } else {
        Err(BuildError::Validation(format!(
            "`{}` {pointer} must be strictly sorted and unique by `{key}`",
            path.display()
        )))
    }
}

fn validate_u64_strings(path: &Path, value: &Value, pointer: &str) -> BuildResult<()> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if key == "extensions" {
                    continue;
                }
                let child_pointer = format!("{pointer}/{key}");
                if matches!(key.as_str(), "memoryBytes" | "size") {
                    let text = child.as_str().ok_or_else(|| {
                        BuildError::Validation(format!(
                            "`{}` {child_pointer} must encode u64 as a decimal string",
                            path.display()
                        ))
                    })?;
                    text.parse::<u64>().map_err(|error| {
                        BuildError::Validation(format!(
                            "`{}` {child_pointer} exceeds u64: {error}",
                            path.display()
                        ))
                    })?;
                }
                validate_u64_strings(path, child, &child_pointer)?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                validate_u64_strings(path, child, &format!("{pointer}/{index}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_versions(path: &Path, value: &Value, pointer: &str) -> BuildResult<()> {
    match value {
        Value::Object(object) => {
            if let Some(product_version) = object.get("productVersion").and_then(Value::as_str) {
                semver::Version::parse(product_version).map_err(|error| {
                    BuildError::Validation(format!(
                        "`{}` {pointer}/productVersion is not SemVer: {error}",
                        path.display()
                    ))
                })?;
            }
            if let (Some(minimum), Some(maximum)) = (
                object.get("minimum").and_then(Value::as_str),
                object.get("maximum").and_then(Value::as_str),
            ) {
                let minimum = parse_contract_range_version(path, pointer, "minimum", minimum)?;
                let maximum = parse_contract_range_version(path, pointer, "maximum", maximum)?;
                if minimum > maximum {
                    return Err(BuildError::Validation(format!(
                        "`{}` {pointer} minimum version exceeds maximum",
                        path.display()
                    )));
                }
            }
            for (key, child) in object {
                if key == "extensions" {
                    continue;
                }
                validate_versions(path, child, &format!("{pointer}/{key}"))?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                validate_versions(path, child, &format!("{pointer}/{index}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_contract_range_version(
    path: &Path,
    pointer: &str,
    field: &str,
    value: &str,
) -> BuildResult<(u32, u32)> {
    let (major, minor) = value.split_once('.').ok_or_else(|| {
        BuildError::Validation(format!(
            "`{}` {pointer}/{field} must be major.minor",
            path.display()
        ))
    })?;
    let major = major.parse::<u32>().map_err(|error| {
        BuildError::Validation(format!(
            "`{}` {pointer}/{field} major exceeds u32: {error}",
            path.display()
        ))
    })?;
    let minor = minor.parse::<u32>().map_err(|error| {
        BuildError::Validation(format!(
            "`{}` {pointer}/{field} minor exceeds u32: {error}",
            path.display()
        ))
    })?;
    Ok((major, minor))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::validate_all;

    #[test]
    fn repository_examples_cover_every_f0_schema() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")));
        assert!(matches!(validate_all(root), Ok(12)));
    }
}
