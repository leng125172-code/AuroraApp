//! R1-08 CLI acceptance, rejection, cardinality, and determinism tests.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use aurora_cli::{Arguments, CliError, execute};
use clap::Parser as _;

const SOURCE: &str = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
PROGRAM Main
VAR
  Index : DINT;
END_VAR
FOR Index := DINT#0 TO DINT#2 BY DINT#1 DO
  Counter := CHECKED_ADD(Counter, DINT#1);
END_FOR;
END_PROGRAM
";

const MAPPING: &str = r#"{
  "kind": "aurora.st-address-mapping",
  "schemaVersion": { "major": 1, "minor": 0, "lifecycle": "preview" },
  "documentId": "01890f3e-4c7b-7cc2-98c4-dc0c0c073900",
  "extensions": { "vendor.test": { "tagCatalog": [], "tagId": "not-a-catalog-entry" } },
  "deviceBindings": [],
  "tagCatalog": [
    {
      "tagId": "01890f3e-4c7b-7cc2-98c4-dc0c0c073901",
      "symbol": "counter"
    }
  ]
}"#;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("aurora-cli-r1-08-{}-{serial}", std::process::id()));
        fs::create_dir(&root).unwrap_or_else(|error| {
            unreachable!(
                "create isolated CLI test directory {}: {error}",
                root.display()
            )
        });
        fs::write(root.join("program.st"), SOURCE)
            .unwrap_or_else(|error| unreachable!("write isolated ST fixture: {error}"));
        fs::write(root.join("mapping.json"), MAPPING)
            .unwrap_or_else(|error| unreachable!("write isolated mapping fixture: {error}"));
        Self { root }
    }

    fn arguments(&self, command: &[&str]) -> Arguments {
        let mut values = vec![OsString::from("aurora-cli")];
        values.extend(command.iter().map(OsString::from));
        values.push(OsString::from("--project-root"));
        values.push(self.root.as_os_str().to_owned());
        Arguments::try_parse_from(values)
            .unwrap_or_else(|error| unreachable!("test command must parse: {error}"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap_or_else(|error| {
            unreachable!(
                "remove isolated CLI test directory {}: {error}",
                self.root.display()
            )
        });
    }
}

fn common(command: &str) -> Vec<&str> {
    vec![
        command,
        "--source",
        "program.st",
        "--mapping",
        "mapping.json",
        "--task",
        "Main=7",
    ]
}

fn file_names(path: &Path) -> Vec<String> {
    let mut names = fs::read_dir(path)
        .unwrap_or_else(|error| unreachable!("list build output: {error}"))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| unreachable!("read build output entry: {error}"))
                .file_name()
                .into_string()
                .unwrap_or_else(|_| unreachable!("test output names are UTF-8"))
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn parse_and_check_emit_canonical_json_without_files() {
    let fixture = Fixture::new();
    let parsed = execute(fixture.arguments(&["parse", "program.st"]))
        .unwrap_or_else(|error| unreachable!("valid source parses: {error}"));
    let ast: serde_json::Value = serde_json::from_slice(&parsed)
        .unwrap_or_else(|error| unreachable!("AST output is JSON: {error}"));
    assert_eq!(ast["schema_version"]["major"], 1);

    let checked = execute(fixture.arguments(&common("check")))
        .unwrap_or_else(|error| unreachable!("valid project checks: {error}"));
    assert_eq!(
        checked,
        br#"{"boundedLoops":1,"deviceBindings":0,"sourceFiles":1,"status":"checked","tags":1,"tasks":1}"#
    );
    assert_eq!(file_names(&fixture.root), ["mapping.json", "program.st"]);
}

#[test]
fn build_publishes_exactly_five_deterministic_artifacts() {
    let fixture = Fixture::new();
    let first = fixture.root.join("first");
    let second = fixture.root.join("second");
    for output in ["first", second.to_str().unwrap_or_else(|| unreachable!())] {
        let mut command = common("build");
        command.extend(["--output", output]);
        execute(fixture.arguments(&command))
            .unwrap_or_else(|error| unreachable!("valid project builds: {error}"));
    }
    let expected = [
        "canonical-ir.json",
        "checkpoint-plan.json",
        "native-source-map.json",
        "program.o",
        "source-map.json",
    ];
    assert_eq!(file_names(&first), expected);
    assert_eq!(file_names(&second), expected);
    for name in expected {
        let left = fs::read(first.join(name))
            .unwrap_or_else(|error| unreachable!("read first artifact: {error}"));
        let right = fs::read(second.join(name))
            .unwrap_or_else(|error| unreachable!("read second artifact: {error}"));
        assert_eq!(
            left, right,
            "artifact {name} changed across identical builds"
        );
    }
}

#[test]
fn rejection_does_not_create_partial_outputs_or_overwrite_existing_files() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("program.st"),
        SOURCE.replace("DINT#2 BY", "Counter BY"),
    )
    .unwrap_or_else(|error| unreachable!("write rejected source: {error}"));
    let rejected = fixture.root.join("rejected");
    let mut command = common("build");
    command.extend([
        "--output",
        rejected.to_str().unwrap_or_else(|| unreachable!()),
    ]);
    let Err(error) = execute(fixture.arguments(&command)) else {
        unreachable!("dynamic loop bound must be rejected before publication");
    };
    assert!(matches!(error, CliError::Diagnostics(_)));
    assert!(error.to_string().contains("ST3001"));
    assert!(!rejected.exists());

    fs::write(fixture.root.join("program.st"), SOURCE)
        .unwrap_or_else(|error| unreachable!("restore accepted source: {error}"));
    let occupied = fixture.root.join("occupied");
    fs::create_dir(&occupied)
        .unwrap_or_else(|error| unreachable!("create occupied output: {error}"));
    fs::write(occupied.join("keep.txt"), "owned by caller")
        .unwrap_or_else(|error| unreachable!("write caller-owned file: {error}"));
    let mut command = common("build");
    command.extend([
        "--output",
        occupied.to_str().unwrap_or_else(|| unreachable!()),
    ]);
    let Err(error) = execute(fixture.arguments(&command)) else {
        unreachable!("nonempty output must be rejected");
    };
    assert!(error.to_string().contains("must be empty"));
    assert_eq!(file_names(&occupied), ["keep.txt"]);

    let mut command = common("build");
    command.extend(["--output", "../outside-project"]);
    let Err(error) = execute(fixture.arguments(&command)) else {
        unreachable!("relative parent traversal must be rejected");
    };
    assert!(error.to_string().contains("normalized child"));
}

#[test]
fn inspect_source_map_returns_only_entries_covering_the_requested_byte() {
    let fixture = Fixture::new();
    let byte_offset = SOURCE
        .find("CHECKED_ADD")
        .unwrap_or_else(|| unreachable!("fixture contains operation"));
    let byte_offset_text = byte_offset.to_string();
    let mut command = vec!["inspect", "source-map"];
    command.extend(common_arguments());
    command.extend([
        "--source-path",
        "program.st",
        "--byte-offset",
        &byte_offset_text,
    ]);
    let output = execute(fixture.arguments(&command))
        .unwrap_or_else(|error| unreachable!("source-map query succeeds: {error}"));
    let query: serde_json::Value = serde_json::from_slice(&output)
        .unwrap_or_else(|error| unreachable!("query output is JSON: {error}"));
    assert_eq!(query["source"]["path"], "program.st");
    assert_eq!(query["byteOffset"], byte_offset);
    assert_eq!(query["symbols"].as_array().map(Vec::len), Some(0));
    assert_eq!(query["nodes"].as_array().map(Vec::len), Some(7));
}

#[test]
fn check_aggregates_each_invalid_source_once_in_canonical_path_order() {
    let fixture = Fixture::new();
    let invalid = "PROGRAM Main\nEND_PROGRAM\n";
    fs::write(fixture.root.join("z.st"), invalid)
        .unwrap_or_else(|error| unreachable!("write invalid z source: {error}"));
    fs::write(fixture.root.join("a.st"), invalid)
        .unwrap_or_else(|error| unreachable!("write invalid a source: {error}"));
    let command = [
        "check", "--source", "z.st", "--source", "a.st", "--task", "Main=7",
    ];
    let Err(CliError::Diagnostics(text)) = execute(fixture.arguments(&command)) else {
        unreachable!("two invalid files must return compiler diagnostics");
    };
    let diagnostics: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| unreachable!("diagnostics are JSON: {error}"));
    let entries = diagnostics
        .as_array()
        .unwrap_or_else(|| unreachable!("diagnostics form an array"));
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["source_path"], "a.st");
    assert_eq!(entries[1]["source_path"], "z.st");
    assert_eq!(entries[0]["code"], "ST0006");
    assert_eq!(entries[1]["code"], "ST0006");
}

#[test]
fn source_read_accepts_the_exact_byte_limit_and_rejects_one_more() {
    const LIMIT: usize = 1024 * 1024;
    let fixture = Fixture::new();
    let base = "AURORA_ST VERSION 1.0;\nPROGRAM Main\nEND_PROGRAM\n";
    let mut source = base.to_owned();
    source.extend(std::iter::repeat_n(' ', LIMIT - base.len()));
    fs::write(fixture.root.join("large.st"), &source)
        .unwrap_or_else(|error| unreachable!("write exact-limit source: {error}"));
    execute(fixture.arguments(&["parse", "large.st"]))
        .unwrap_or_else(|error| unreachable!("exact-limit source parses: {error}"));

    source.push(' ');
    fs::write(fixture.root.join("large.st"), source)
        .unwrap_or_else(|error| unreachable!("write over-limit source: {error}"));
    let Err(error) = execute(fixture.arguments(&["parse", "large.st"])) else {
        unreachable!("one byte over the source limit must be rejected");
    };
    assert!(error.to_string().contains("exceeds byte limit 1048576"));
}

fn common_arguments() -> Vec<&'static str> {
    vec![
        "--source",
        "program.st",
        "--mapping",
        "mapping.json",
        "--task",
        "Main=7",
    ]
}
