//! Cyclic Workflow 规范源的 host-only 完整性门禁。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{BuildError, BuildResult};

const LANGUAGE_PATH: &str = "Sources/Contracts/workflow/v1/language.md";
const LAYOUT_PATH: &str = "Sources/Contracts/workflow/v1/layout.md";
const TRACE_LAYOUT_PATH: &str = "Sources/Contracts/workflow/v1/trace-layout.md";

const REQUIRED_NODE_KINDS: &str = "
Entry Action Decision Fork Join Wait Subworkflow End
";

const REQUIRED_TRACE_EVENTS: &str = "
WorkflowInitialized NodeExecuted TransitionTaken ForkActivated JoinSatisfied WaitObserved
CancelRequested CancelApplied SubworkflowActivated SubworkflowCompleted OutputStaged WatchedValue
CompletionRequested WorkflowCompleted WorkflowFaulted ForceObserved FallbackObserved DeadlineObserved
ScanCommitted ScanDiscarded
";

const REQUIRED_DIAGNOSTICS: &str = "
WF0001 WF0002 WF0003 WF0004 WF0005 WF0006 WF0007 WF0008 WF0009 WF0010 WF0011 WF0012
WF1001 WF1002 WF1003 WF1004 WF1005 WF1006 WF1007 WF1008 WF1009 WF1010 WF1011 WF1012 WF1013 WF1014
WF2001 WF2002 WF2003 WF2004 WF2005 WF2006 WF2007 WF2008 WF2009 WF2010 WF2011 WF2012 WF2013 WF2014 WF2015 WF2016
WF3001 WF3002 WF3003 WF3004 WF3005 WF3006 WF3007 WF3008 WF3009 WF3010 WF3011
WFF0001 WFF0002 WFF0003 WFF0004 WFF0005
";

const REQUIRED_LANGUAGE_CLAUSES: &[&str] = &[
    "作者格式：YAML 1.2",
    "YAML 1.2 Core Schema",
    "RFC 8785 JCS",
    "每个 Workflow 恰有一个 Entry",
    "除 Entry 和 Join 外的节点恰有一个控制入边",
    "executionOrder",
    "单一静态写者规则",
    "`Merge` 专用于 Decision",
    "condition 优先",
    "maxTraversalsPerRun",
    "CancelOthers",
    "KeepRunning",
    "WaitAtBoundary",
    "不创建线程",
    "DropNewest",
    "locale-neutral",
];

/// 验证 R2-00 规范文件、冻结目录和二进制布局没有遗漏、重复或偏移漂移。
pub(crate) fn validate(repository_root: &Path) -> BuildResult<()> {
    let language = read(repository_root.join(LANGUAGE_PATH))?;
    let layout = read(repository_root.join(LAYOUT_PATH))?;
    let trace_layout = read(repository_root.join(TRACE_LAYOUT_PATH))?;
    validate_sources(&language, &layout, &trace_layout)
}

fn read(path: PathBuf) -> BuildResult<String> {
    fs::read_to_string(&path).map_err(|source| BuildError::Io {
        operation: "read Cyclic Workflow specification",
        path,
        source,
    })
}

fn validate_sources(language: &str, layout: &str, trace_layout: &str) -> BuildResult<()> {
    validate_required_clauses(language, layout, trace_layout)?;
    validate_catalog(
        language,
        "## 4. Graph 结构",
        "## 5. 扫描与事务语义",
        2,
        &words(REQUIRED_NODE_KINDS),
        "node kind",
    )?;
    validate_catalog(
        trace_layout,
        "## 4. Event kind",
        "### 4.1 EventDetail",
        2,
        &words(REQUIRED_TRACE_EVENTS),
        "Trace event",
    )?;
    validate_diagnostics(language, layout, trace_layout)?;
    validate_trace_layout(trace_layout)
}

fn validate_required_clauses(language: &str, layout: &str, trace_layout: &str) -> BuildResult<()> {
    for clause in REQUIRED_LANGUAGE_CLAUSES {
        if !language.contains(clause) {
            return Err(BuildError::Validation(format!(
                "Cyclic Workflow Preview 1.0 specification is missing required clause `{clause}`"
            )));
        }
    }
    for clause in [
        "不是控制契约",
        "不得改变 Workflow",
        "signed `i32` canvas units",
        "不写入共享 Layout",
    ] {
        if !layout.contains(clause) {
            return Err(BuildError::Validation(format!(
                "Workflow Layout Preview 1.0 specification is missing required clause `{clause}`"
            )));
        }
    }
    for clause in [
        "Header：96 bytes",
        "Record：192 bytes",
        "AURWFT01",
        "AURWFR01",
        "DropNewest",
        "consumer 停止",
        "1=JoinAll",
        "3=Merge",
        "非零 `u16`（最多 65535 项）",
        "5=TimedOut",
        "4=FinishAfterDeadline",
        "ScanCommitted` 必须为 before+1",
    ] {
        if !trace_layout.contains(clause) {
            return Err(BuildError::Validation(format!(
                "Workflow Trace Preview 1.0 specification is missing required clause `{clause}`"
            )));
        }
    }
    Ok(())
}

fn words(source: &str) -> BTreeSet<String> {
    source.split_ascii_whitespace().map(str::to_owned).collect()
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> BuildResult<&'a str> {
    let (_, after_start) = source.split_once(start).ok_or_else(|| {
        BuildError::Validation(format!(
            "Workflow specification is missing section `{start}`"
        ))
    })?;
    let (body, _) = after_start.split_once(end).ok_or_else(|| {
        BuildError::Validation(format!(
            "Workflow specification section `{start}` has no `{end}` boundary"
        ))
    })?;
    Ok(body)
}

fn validate_catalog(
    source: &str,
    start: &str,
    end: &str,
    column: usize,
    expected: &BTreeSet<String>,
    kind: &str,
) -> BuildResult<()> {
    let body = section(source, start, end)?;
    let mut actual = BTreeSet::new();
    for line in body.lines() {
        let Some(value) = table_backtick_cell(line, column) else {
            continue;
        };
        if !actual.insert(value.to_owned()) {
            return Err(BuildError::Validation(format!(
                "Cyclic Workflow {kind} `{value}` is defined more than once"
            )));
        }
    }
    if &actual != expected {
        let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
        let extra = actual.difference(expected).cloned().collect::<Vec<_>>();
        return Err(BuildError::Validation(format!(
            "Cyclic Workflow Preview 1.0 {kind} catalog differs: missing [{}], extra [{}]",
            missing.join(", "),
            extra.join(", ")
        )));
    }
    Ok(())
}

fn table_backtick_cell(line: &str, column: usize) -> Option<&str> {
    let cell = line.split('|').nth(column)?.trim();
    cell.strip_prefix('`')?.strip_suffix('`')
}

fn validate_diagnostics(language: &str, layout: &str, trace_layout: &str) -> BuildResult<()> {
    let catalog = section(
        language,
        "## 9. 稳定诊断目录",
        "## 10. 诊断排序与 cardinality",
    )?;
    let mut definitions = BTreeSet::new();
    for line in catalog.lines() {
        let Some(code) = table_backtick_cell(line, 1) else {
            continue;
        };
        if is_diagnostic_code(code) && !definitions.insert(code.to_owned()) {
            return Err(BuildError::Validation(format!(
                "Cyclic Workflow diagnostic `{code}` is defined more than once"
            )));
        }
    }

    let expected = words(REQUIRED_DIAGNOSTICS);
    if definitions != expected {
        let missing = expected
            .difference(&definitions)
            .cloned()
            .collect::<Vec<_>>();
        let extra = definitions
            .difference(&expected)
            .cloned()
            .collect::<Vec<_>>();
        return Err(BuildError::Validation(format!(
            "Cyclic Workflow Preview 1.0 diagnostic catalog differs: missing [{}], extra [{}]",
            missing.join(", "),
            extra.join(", ")
        )));
    }

    for source in [language, layout, trace_layout] {
        for code in diagnostic_references(source) {
            if !definitions.contains(code) {
                return Err(BuildError::Validation(format!(
                    "Cyclic Workflow specification references undefined diagnostic `{code}`"
                )));
            }
        }
    }
    Ok(())
}

fn diagnostic_references(source: &str) -> impl Iterator<Item = &str> {
    source.match_indices("WF").filter_map(|(start, _)| {
        let suffix = &source[start..];
        let length = if suffix.as_bytes().get(2) == Some(&b'F') {
            7
        } else {
            6
        };
        suffix.get(..length).filter(|code| is_diagnostic_code(code))
    })
}

fn is_diagnostic_code(value: &str) -> bool {
    let digits = value
        .strip_prefix("WFF")
        .or_else(|| value.strip_prefix("WF"));
    digits
        .is_some_and(|digits| digits.len() == 4 && digits.bytes().all(|byte| byte.is_ascii_digit()))
}

fn validate_trace_layout(trace_layout: &str) -> BuildResult<()> {
    let header = section(
        trace_layout,
        "## 1. 文件 Header",
        "## 2. Record flags 与 sentinel",
    )?;
    validate_contiguous_layout(header, 96, "Workflow Trace header")?;
    let record = section(trace_layout, "## 3. 固定 Record", "## 4. Event kind")?;
    validate_contiguous_layout(record, 192, "Workflow Trace record")
}

fn validate_contiguous_layout(source: &str, expected_size: usize, kind: &str) -> BuildResult<()> {
    let mut expected_offset = 0_usize;
    let mut fields = 0_usize;
    for line in source.lines() {
        let mut cells = line.split('|').map(str::trim);
        let _empty = cells.next();
        let Some(offset_text) = cells.next() else {
            continue;
        };
        let Some(size_text) = cells.next() else {
            continue;
        };
        let (Ok(offset), Ok(size)) = (offset_text.parse::<usize>(), size_text.parse::<usize>())
        else {
            continue;
        };
        if size == 0 || offset != expected_offset {
            return Err(BuildError::Validation(format!(
                "{kind} has a gap, overlap, or zero-size field at offset {offset}; expected {expected_offset}"
            )));
        }
        expected_offset = expected_offset
            .checked_add(size)
            .ok_or_else(|| BuildError::Validation(format!("{kind} size arithmetic overflowed")))?;
        fields = fields.saturating_add(1);
    }
    if fields == 0 || expected_offset != expected_size {
        return Err(BuildError::Validation(format!(
            "{kind} ends at {expected_offset} bytes instead of {expected_size}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_sources;

    const LANGUAGE: &str = include_str!("../../../../Contracts/workflow/v1/language.md");
    const LAYOUT: &str = include_str!("../../../../Contracts/workflow/v1/layout.md");
    const TRACE_LAYOUT: &str = include_str!("../../../../Contracts/workflow/v1/trace-layout.md");

    #[test]
    fn checked_in_workflow_specs_are_complete_and_unambiguous() {
        let result = validate_sources(LANGUAGE, LAYOUT, TRACE_LAYOUT);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn missing_node_kind_is_rejected() {
        let broken = LANGUAGE.replace("| 8 | `End` |", "| 8 | `Finish` |");
        assert!(validate_sources(&broken, LAYOUT, TRACE_LAYOUT).is_err());
    }

    #[test]
    fn ambiguous_entry_or_merge_contract_is_rejected() {
        let broken_entry = LANGUAGE.replace("除 Entry 和 Join 外", "除 Join 外");
        assert!(validate_sources(&broken_entry, LAYOUT, TRACE_LAYOUT).is_err());

        let broken_merge = LANGUAGE.replace("`Merge` 专用于 Decision", "`Merge` 用于 Decision");
        assert!(validate_sources(&broken_merge, LAYOUT, TRACE_LAYOUT).is_err());
    }

    #[test]
    fn duplicate_or_missing_diagnostic_is_rejected() {
        let broken = LANGUAGE.replace(
            "| `WF3011` | LayoutLimitExceeded |",
            "| `WF3010` | LayoutLimitExceeded |",
        );
        assert!(validate_sources(&broken, LAYOUT, TRACE_LAYOUT).is_err());
    }

    #[test]
    fn undefined_diagnostic_reference_is_rejected() {
        let broken = LAYOUT.replace("`WF3011`", "`WF3999`");
        assert!(validate_sources(LANGUAGE, &broken, TRACE_LAYOUT).is_err());
    }

    #[test]
    fn trace_layout_gap_is_rejected() {
        let broken =
            TRACE_LAYOUT.replace("| 184 | 8 | Reserved `0` |", "| 185 | 8 | Reserved `0` |");
        assert!(validate_sources(LANGUAGE, LAYOUT, &broken).is_err());
    }
}
