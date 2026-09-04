//! Command-line surface and verification orchestration.

use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use clap::{Parser, Subcommand};

use crate::digest::canonical_json_digest;
use crate::error::{BuildError, BuildResult};
use crate::{schema, supply_chain};

/// Arguments accepted by the repository verification entry point.
#[derive(Debug, Parser)]
#[command(name = "aurora-build", version, about)]
pub(crate) struct Arguments {
    /// Operation to perform.
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// Supported host-side build operations.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Validate all F0 contract schemas and golden examples.
    Schemas,
    /// Print the RFC 8785 canonical SHA-256 digest of a JSON document.
    Digest {
        /// JSON document to canonicalize and hash.
        input: PathBuf,
    },
    /// Run cross-language contract tests.
    Contracts,
    /// Generate a deterministic `CycloneDX` 1.6 software bill of materials.
    Sbom {
        /// Output path, relative to the repository root unless absolute.
        #[arg(long, default_value = "Builds/sbom.cdx.json")]
        output: PathBuf,
    },
    /// Generate deterministic SLSA v1 build provenance for repository inputs.
    Provenance {
        /// Output path, relative to the repository root unless absolute.
        #[arg(long, default_value = "Builds/provenance.intoto.json")]
        output: PathBuf,
        /// Artifact attested as the build output, relative to the repository root unless absolute.
        #[arg(long, default_value = "Builds/sbom.cdx.json")]
        subject: PathBuf,
    },
    /// Run the cross-platform local F0 core quality gate.
    Verify,
}

/// Execute one parsed command and return its single-line summary.
pub(crate) fn execute(command: Command) -> BuildResult<String> {
    let repository_root = repository_root()?;
    match command {
        Command::Schemas => {
            let count = schema::validate_all(&repository_root)?;
            Ok(format!("validated 4 schemas and {count} examples"))
        }
        Command::Digest { input } => {
            let path = resolve_path(&repository_root, &input);
            canonical_json_digest(&path)
        }
        Command::Contracts => {
            run_contract_tests(&repository_root)?;
            Ok("cross-language contract tests passed".to_owned())
        }
        Command::Sbom { output } => {
            let path = resolve_path(&repository_root, &output);
            let count = supply_chain::generate_sbom(&repository_root, &path)?;
            Ok(format!(
                "wrote CycloneDX SBOM with {count} components to {}",
                path.display()
            ))
        }
        Command::Provenance { output, subject } => {
            let path = resolve_path(&repository_root, &output);
            let subject_path = resolve_path(&repository_root, &subject);
            supply_chain::generate_provenance(&repository_root, &path, &subject_path)?;
            Ok(format!("wrote SLSA provenance to {}", path.display()))
        }
        Command::Verify => {
            run_verification(&repository_root)?;
            Ok("F0 verification passed".to_owned())
        }
    }
}

fn repository_root() -> BuildResult<PathBuf> {
    let manifest_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_directory
        .ancestors()
        .nth(4)
        .ok_or_else(|| BuildError::Validation("cannot locate repository root".to_owned()))?;
    if root.join("Sources/Rust/Cargo.toml").is_file() && root.join("VERSION").is_file() {
        Ok(root.to_path_buf())
    } else {
        Err(BuildError::Validation(format!(
            "resolved repository root `{}` is missing F0 sentinels",
            root.display()
        )))
    }
}

fn resolve_path(repository_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repository_root.join(path)
    }
}

fn run_contract_tests(repository_root: &Path) -> BuildResult<()> {
    run(
        repository_root,
        "cargo",
        &[
            "test",
            "--locked",
            "--manifest-path",
            "Sources/Rust/Cargo.toml",
            "-p",
            "aurora-control-contracts",
        ],
    )?;
    run(
        repository_root,
        "dotnet",
        &[
            "test",
            "Sources/DotNet/Aurora.slnx",
            "--configuration",
            "Release",
            "--no-restore",
        ],
    )
}

fn run_verification(repository_root: &Path) -> BuildResult<()> {
    schema::validate_all(repository_root)?;
    run(
        repository_root,
        "cargo",
        &[
            "fmt",
            "--manifest-path",
            "Sources/Rust/Cargo.toml",
            "--all",
            "--",
            "--check",
        ],
    )?;
    run(
        repository_root,
        "cargo",
        &[
            "clippy",
            "--locked",
            "--manifest-path",
            "Sources/Rust/Cargo.toml",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    run(
        repository_root,
        "cargo",
        &[
            "test",
            "--locked",
            "--manifest-path",
            "Sources/Rust/Cargo.toml",
            "--workspace",
        ],
    )?;
    run(
        repository_root,
        "cargo",
        &[
            "check",
            "--locked",
            "--manifest-path",
            "Sources/Rust/Cargo.toml",
            "--workspace",
            "--target",
            "x86_64-unknown-linux-gnu",
        ],
    )?;
    run(
        repository_root,
        "dotnet",
        &[
            "test",
            "Sources/DotNet/Aurora.slnx",
            "--configuration",
            "Release",
            "--no-restore",
        ],
    )?;
    let builds = repository_root.join("Builds");
    let sbom = builds.join("sbom.cdx.json");
    supply_chain::generate_sbom(repository_root, &sbom)?;
    supply_chain::generate_provenance(
        repository_root,
        &builds.join("provenance.intoto.json"),
        &sbom,
    )
}

fn run(repository_root: &Path, program: &str, arguments: &[&str]) -> BuildResult<()> {
    let display = format!("{program} {}", arguments.join(" "));
    let status = ProcessCommand::new(program)
        .args(arguments)
        .current_dir(repository_root)
        .status()
        .map_err(|source| BuildError::StartProcess {
            program: display.clone(),
            source,
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(BuildError::ProcessFailed {
            program: display,
            status: status.code().map_or_else(
                || "terminated by signal".to_owned(),
                |code| code.to_string(),
            ),
        })
    }
}
