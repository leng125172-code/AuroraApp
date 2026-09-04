//! Deterministic `CycloneDX` and SLSA supply-chain documents.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Component, Path};
use std::process::Command;

use serde_json::{Value, json};

use crate::digest::{sha256, write_json};
use crate::error::{BuildError, BuildResult};

/// Generate a deterministic `CycloneDX` 1.6 SBOM from locked Rust and .NET graphs.
pub(crate) fn generate_sbom(repository_root: &Path, output: &Path) -> BuildResult<usize> {
    let metadata = command_json(
        repository_root,
        "cargo",
        &[
            "metadata",
            "--manifest-path",
            "Sources/Rust/Cargo.toml",
            "--format-version",
            "1",
            "--locked",
        ],
    )?;
    let mut components = BTreeMap::<String, Value>::new();
    if let Some(packages) = metadata.get("packages").and_then(Value::as_array) {
        for package in packages {
            let Some(name) = package.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(version) = package.get("version").and_then(Value::as_str) else {
                continue;
            };
            let purl = format!("pkg:cargo/{name}@{version}");
            let mut component = json!({
                "bom-ref": purl,
                "type": if package.get("source").is_some_and(Value::is_null) { "application" } else { "library" },
                "name": name,
                "version": version,
                "purl": purl
            });
            if let Some(license) = package.get("license").and_then(Value::as_str) {
                component["licenses"] = json!([{ "expression": license }]);
            }
            components.insert(purl, component);
        }
    }
    collect_dotnet_components(repository_root, &mut components)?;

    let version = read_trimmed(&repository_root.join("VERSION"))?;
    let document = json!({
        "$schema": "https://cyclonedx.org/schema/bom-1.6.schema.json",
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "version": 1,
        "metadata": {
            "component": {
                "bom-ref": format!("pkg:generic/aurora@{version}"),
                "type": "application",
                "name": "Aurora",
                "version": version,
                "licenses": [{ "license": { "name": "Proprietary" } }]
            },
            "tools": {
                "components": [{
                    "type": "application",
                    "name": "aurora-build",
                    "version": env!("CARGO_PKG_VERSION")
                }]
            }
        },
        "components": components.into_values().collect::<Vec<_>>()
    });
    let count = document["components"].as_array().map_or(0, Vec::len);
    write_json(output, &document)?;
    Ok(count)
}

/// Generate an in-toto statement using the SLSA v1 provenance predicate.
pub(crate) fn generate_provenance(
    repository_root: &Path,
    output: &Path,
    subject: &Path,
) -> BuildResult<()> {
    let subject_bytes = fs::read(subject).map_err(|source| BuildError::Io {
        operation: "read provenance subject",
        path: subject.to_path_buf(),
        source,
    })?;
    let subject_name = resource_name(repository_root, subject)?;
    let subject_digest = sha256(&subject_bytes);
    let mut dependencies = Vec::new();
    dependencies.push(source_material(repository_root)?);
    dependencies.extend(input_materials(repository_root)?);
    let statement =
        provenance_statement(&subject_name, &subject_digest, builder_id(), &dependencies);
    write_json(output, &statement)
}

fn provenance_statement(
    subject_name: &str,
    subject_digest: &str,
    builder_id: &str,
    resolved_dependencies: &[Value],
) -> Value {
    json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{
            "name": subject_name,
            "digest": { "sha256": subject_digest.trim_start_matches("sha256:") }
        }],
        "predicateType": "https://slsa.dev/provenance/v1",
        "predicate": {
            "buildDefinition": {
                "buildType": "urn:caymir:aurora:build-type:f0",
                "externalParameters": { "configuration": "F0", "target": "x86_64-unknown-linux-gnu" },
                "internalParameters": {},
                "resolvedDependencies": resolved_dependencies
            },
            "runDetails": {
                "builder": { "id": builder_id },
                "byproducts": []
            }
        }
    })
}

fn builder_id() -> &'static str {
    if env::var("GITHUB_ACTIONS").is_ok_and(|value| value == "true") {
        "urn:caymir:aurora:builder:github-actions"
    } else {
        "urn:caymir:aurora:builder:local"
    }
}

fn source_material(repository_root: &Path) -> BuildResult<Value> {
    let status = command_output(
        repository_root,
        "git",
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if status.is_empty() {
        let commit = command_text(repository_root, "git", &["rev-parse", "HEAD"])?;
        let remote = command_text(
            repository_root,
            "git",
            &["config", "--get", "remote.origin.url"],
        )?;
        Ok(json!({
            "uri": canonical_repository_uri(&remote),
            "digest": { "gitCommit": commit }
        }))
    } else {
        Ok(json!({
            "name": "aurora-working-tree",
            "digest": { "sha256": working_tree_digest(repository_root)? }
        }))
    }
}

fn working_tree_digest(repository_root: &Path) -> BuildResult<String> {
    let output = command_output(
        repository_root,
        "git",
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let mut paths = output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            std::str::from_utf8(path)
                .map(str::to_owned)
                .map_err(|error| {
                    BuildError::Validation(format!("repository path is not UTF-8: {error}"))
                })
        })
        .collect::<BuildResult<Vec<_>>>()?;
    paths.sort();

    let mut entries = Vec::with_capacity(paths.len());
    for relative_path in paths {
        let relative = Path::new(&relative_path);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(BuildError::Validation(format!(
                "git returned unsafe repository path `{relative_path}`"
            )));
        }
        let path = repository_root.join(relative);
        let bytes = fs::read(&path).map_err(|source| BuildError::Io {
            operation: "read working-tree input",
            path,
            source,
        })?;
        entries.push(json!({
            "uri": relative_path.replace('\\', "/"),
            "digest": { "sha256": sha256(&bytes).trim_start_matches("sha256:") }
        }));
    }
    let canonical = serde_jcs::to_vec(&entries).map_err(|error| {
        BuildError::Validation(format!("canonicalize working-tree inputs: {error}"))
    })?;
    Ok(sha256(&canonical).trim_start_matches("sha256:").to_owned())
}

fn canonical_repository_uri(remote: &str) -> String {
    let github_path = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))
        .or_else(|| remote.strip_prefix("https://github.com/"));
    if let Some(path) = github_path {
        format!(
            "https://github.com/{}",
            path.trim_end_matches('/').trim_end_matches(".git")
        )
    } else {
        remote
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .to_owned()
    }
}

fn resource_name(repository_root: &Path, path: &Path) -> BuildResult<String> {
    let resource = path.strip_prefix(repository_root).unwrap_or_else(|_| {
        path.file_name()
            .map_or_else(|| Path::new("subject"), Path::new)
    });
    let name = resource.to_str().ok_or_else(|| {
        BuildError::Validation(format!(
            "provenance subject path is not UTF-8: {}",
            path.display()
        ))
    })?;
    Ok(name.replace('\\', "/"))
}

fn collect_dotnet_components(
    repository_root: &Path,
    components: &mut BTreeMap<String, Value>,
) -> BuildResult<()> {
    let contracts_root = repository_root.join("Sources/DotNet");
    for lock_path in [
        contracts_root.join("Aurora.Contracts/packages.lock.json"),
        contracts_root.join("Aurora.Contracts.Tests/packages.lock.json"),
    ] {
        let lock = crate::digest::read_json(&lock_path)?;
        let Some(frameworks) = lock.get("dependencies").and_then(Value::as_object) else {
            continue;
        };
        for dependencies in frameworks.values().filter_map(Value::as_object) {
            for (name, details) in dependencies {
                let Some(version) = details.get("resolved").and_then(Value::as_str) else {
                    continue;
                };
                let purl = format!("pkg:nuget/{name}@{version}");
                components.entry(purl.clone()).or_insert_with(|| {
                    json!({
                        "bom-ref": purl,
                        "type": "library",
                        "name": name,
                        "version": version,
                        "purl": purl
                    })
                });
            }
        }
    }
    Ok(())
}

fn input_materials(repository_root: &Path) -> BuildResult<Vec<Value>> {
    let relative_paths = [
        "VERSION",
        "global.json",
        "rust-toolchain.toml",
        "Directory.Packages.props",
        "Sources/Rust/Cargo.lock",
        "Sources/DotNet/Aurora.Contracts/packages.lock.json",
        "Sources/DotNet/Aurora.Contracts.Tests/packages.lock.json",
    ];
    let mut materials = Vec::with_capacity(relative_paths.len());
    for relative_path in relative_paths {
        let path = repository_root.join(relative_path);
        let bytes = fs::read(&path).map_err(|source| BuildError::Io {
            operation: "read provenance input",
            path,
            source,
        })?;
        materials.push(json!({
            "uri": relative_path.replace('\\', "/"),
            "digest": { "sha256": sha256(&bytes).trim_start_matches("sha256:") }
        }));
    }
    Ok(materials)
}

fn read_trimmed(path: &Path) -> BuildResult<String> {
    fs::read_to_string(path)
        .map(|text| text.trim().to_owned())
        .map_err(|source| BuildError::Io {
            operation: "read",
            path: path.to_path_buf(),
            source,
        })
}

fn command_json(repository_root: &Path, program: &str, arguments: &[&str]) -> BuildResult<Value> {
    let output = command_output(repository_root, program, arguments)?;
    serde_json::from_slice(&output).map_err(|source| BuildError::Json {
        path: Path::new(program).to_path_buf(),
        source,
    })
}

fn command_text(repository_root: &Path, program: &str, arguments: &[&str]) -> BuildResult<String> {
    let output = command_output(repository_root, program, arguments)?;
    String::from_utf8(output)
        .map(|text| text.trim().to_owned())
        .map_err(|error| {
            BuildError::Validation(format!("`{program}` emitted non-UTF-8 output: {error}"))
        })
}

fn command_output(
    repository_root: &Path,
    program: &str,
    arguments: &[&str],
) -> BuildResult<Vec<u8>> {
    let display = format!("{program} {}", arguments.join(" "));
    let output = Command::new(program)
        .args(arguments)
        .current_dir(repository_root)
        .output()
        .map_err(|source| BuildError::StartProcess {
            program: display.clone(),
            source,
        })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(BuildError::ProcessFailed {
            program: display,
            status: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use std::path::Path;

    use super::{canonical_repository_uri, provenance_statement, resource_name};

    #[test]
    fn provenance_attests_the_output_and_classifies_inputs_as_dependencies() {
        let statement = provenance_statement(
            "Builds/sbom.cdx.json",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "urn:caymir:aurora:builder:local",
            &[json!({
                "name": "aurora-working-tree",
                "digest": { "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }
            })],
        );
        assert_eq!(statement["subject"][0]["name"], "Builds/sbom.cdx.json");
        assert_eq!(
            statement["predicate"]["buildDefinition"]["resolvedDependencies"][0]["name"],
            "aurora-working-tree"
        );
        assert_eq!(
            statement["predicate"]["runDetails"]["builder"]["id"],
            "urn:caymir:aurora:builder:local"
        );
        assert_eq!(
            statement["predicate"]["runDetails"]["byproducts"],
            json!([])
        );
    }

    #[test]
    fn github_clone_forms_have_one_canonical_repository_uri() {
        for remote in [
            "git@github.com:leng125172-code/AuroraApp.git",
            "ssh://git@github.com/leng125172-code/AuroraApp.git",
            "https://github.com/leng125172-code/AuroraApp.git",
        ] {
            assert_eq!(
                canonical_repository_uri(remote),
                "https://github.com/leng125172-code/AuroraApp"
            );
        }
    }

    #[test]
    fn external_subject_paths_do_not_leak_machine_specific_directories() {
        let name = resource_name(Path::new("repository"), Path::new("other/sbom.cdx.json"));
        assert!(matches!(name.as_deref(), Ok("sbom.cdx.json")));
    }
}
