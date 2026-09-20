//! R3 Guardian 与现场协议规范源的 host-only 完整性门禁。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{BuildError, BuildResult};

const GUARDIAN_PATH: &str = "Sources/Contracts/io/v1/guardian-contract.md";
const PROTOCOL_PATH: &str = "Sources/Contracts/io/v1/protocol-matrix.md";
const TARGET_PROFILE_PATH: &str = "Sources/Contracts/io/v1/target-profile.md";

const REGION_FIELDS: &[(&str, usize, usize)] = &[
    ("Magic", 0, 8),
    ("LayoutMajor", 8, 2),
    ("LayoutMinor", 10, 2),
    ("HeaderBytes", 12, 4),
    ("TotalBytes", 16, 8),
    ("GuardianEpoch", 24, 8),
    ("ConfigurationDigest", 32, 32),
    ("InputOffset", 64, 8),
    ("InputStrideBytes", 72, 4),
    ("InputPayloadCapacityBytes", 76, 4),
    ("OutputOffset", 80, 8),
    ("OutputStrideBytes", 88, 4),
    ("OutputPayloadCapacityBytes", 92, 4),
    ("ValueCount", 96, 4),
    ("InputGroupCount", 100, 2),
    ("OutputGroupCount", 102, 2),
    ("Flags", 104, 8),
    ("CapabilityDigest", 112, 32),
    ("LeaseId", 144, 16),
    ("InputPublishToken", 160, 8),
    ("OutputPublishToken", 168, 8),
    ("InputDropCount", 176, 8),
    ("OutputRejectCount", 184, 8),
    ("Reserved", 192, 64),
];

const SLOT_FIELDS: &[(&str, usize, usize)] = &[
    ("Generation", 0, 8),
    ("GuardianEpoch", 8, 8),
    ("ImageSequence", 16, 8),
    ("SourceMonotonicNs", 24, 8),
    ("PublishMonotonicNs", 32, 8),
    ("UtcSeconds", 40, 8),
    ("UtcNanoseconds", 48, 4),
    ("TimeQuality", 52, 1),
    ("AggregateQuality", 53, 1),
    ("StatusFlags", 54, 2),
    ("ValueCount", 56, 4),
    ("PayloadBytes", 60, 4),
    ("LayoutDigest", 64, 32),
    ("DroppedBefore", 96, 8),
    ("DiagnosticsOffset", 104, 4),
    ("DiagnosticsBytes", 108, 4),
    ("Reserved", 112, 16),
];

const ERROR_CODES: &str = "
IO0001 IO0002 IO0003 IO0004 IO0005 IO0006 IO0007 IO0008
IO1001 IO1002 IO1003 IO1004
IO2001 IO2002 IO2003 IO2004 IO2005 IO2006
IO3001 IO3002 IO3003
";

/// 验证 R3-00 规范、固定布局、协议角色和 Target Profile 证据没有遗漏或漂移。
pub(crate) fn validate(repository_root: &Path) -> BuildResult<()> {
    let guardian = read(repository_root.join(GUARDIAN_PATH))?;
    let protocol = read(repository_root.join(PROTOCOL_PATH))?;
    let target_profile = read(repository_root.join(TARGET_PROFILE_PATH))?;
    validate_sources(&guardian, &protocol, &target_profile)
}

fn read(path: PathBuf) -> BuildResult<String> {
    fs::read_to_string(&path).map_err(|source| BuildError::Io {
        operation: "read Guardian/I/O specification",
        path,
        source,
    })
}

fn validate_sources(guardian: &str, protocol: &str, target_profile: &str) -> BuildResult<()> {
    validate_required_clauses(guardian, protocol, target_profile)?;
    validate_layout(
        section(
            guardian,
            "### 5.1 Region header",
            "### 5.2 Image slot header",
        )?,
        REGION_FIELDS,
        256,
        "region header",
    )?;
    validate_layout(
        section(guardian, "### 5.2 Image slot header", "## 6. I/O value")?,
        SLOT_FIELDS,
        128,
        "image slot header",
    )?;
    validate_error_codes(guardian)
}

fn validate_required_clauses(
    guardian: &str,
    protocol: &str,
    target_profile: &str,
) -> BuildResult<()> {
    for clause in [
        "AURIO001",
        "Acquire",
        "Release",
        "N/N-1",
        "FallbackArmed → LeaseRevoked → OldBackendQuiesced",
        "preferred_backend",
        "approved_fallback_backends",
        "禁止由 jitter、单次 timeout 或 backend fault 自动选择下一后端",
        "每个 interface 同时恰有一个 backend owner",
        "不添加 EtherCrab/IgH 依赖",
    ] {
        require(guardian, clause, "Guardian Contract")?;
    }

    for clause in [
        "EtherCAT | MainDevice",
        "Modbus TCP | Client",
        "Modbus RTU | Master",
        "CAN 2.0 | SocketCAN raw",
        "CAN FD | SocketCAN raw",
        "LIN | Controller + fixed schedule",
        "PDU `<=253` bytes",
        "ADU `<=260` bytes",
        "ADU `<=256` bytes",
        "classic payload `0..=8` bytes",
        "payload `0..=64` bytes",
        "FSoE",
        "OPC UA、MQTT 5 与 Sparkplug B",
        "同一 interface 任意时刻只有一个 owner",
    ] {
        require(protocol, clause, "protocol matrix")?;
    }

    for clause in [
        "Dell Precision 7920 Tower",
        "p50、p99.9、max",
        "baseline",
        "tuned",
        "完整 kernel cmdline",
        "preferred_backend=ethercrab",
        "approved_fallback_backends",
        "maximum_latch_attempts=2",
        "Preview 1.0 不允许自动切换策略",
        "Cargo.lock",
        "IgH 若安装",
        "不构成功能安全、硬实时、跨硬件性能",
    ] {
        require(target_profile, clause, "I/O Target Profile")?;
    }
    Ok(())
}

fn require(source: &str, clause: &str, document: &str) -> BuildResult<()> {
    if source.contains(clause) {
        Ok(())
    } else {
        Err(BuildError::Validation(format!(
            "{document} is missing required clause `{clause}`"
        )))
    }
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> BuildResult<&'a str> {
    let (_, after_start) = source.split_once(start).ok_or_else(|| {
        BuildError::Validation(format!(
            "Guardian specification is missing section `{start}`"
        ))
    })?;
    let (body, _) = after_start.split_once(end).ok_or_else(|| {
        BuildError::Validation(format!(
            "Guardian specification section `{start}` has no `{end}` boundary"
        ))
    })?;
    Ok(body)
}

fn validate_layout(
    source: &str,
    expected: &[(&str, usize, usize)],
    expected_bytes: usize,
    name: &str,
) -> BuildResult<()> {
    let mut actual = BTreeMap::new();
    for line in source.lines() {
        let cells = table_cells(line);
        if cells.len() < 3 {
            continue;
        }
        let Ok(offset) = cells[0].parse::<usize>() else {
            continue;
        };
        let size = cells[1]
            .parse::<usize>()
            .map_err(|_| BuildError::Validation(format!("{name} has invalid size in `{line}`")))?;
        let field = cells[2].to_owned();
        if actual.insert(field.clone(), (offset, size)).is_some() {
            return Err(BuildError::Validation(format!(
                "{name} repeats field `{field}`"
            )));
        }
    }

    let expected_names = expected
        .iter()
        .map(|(field, _, _)| (*field).to_owned())
        .collect::<BTreeSet<_>>();
    let actual_names = actual.keys().cloned().collect::<BTreeSet<_>>();
    if actual_names != expected_names {
        let missing = expected_names
            .difference(&actual_names)
            .cloned()
            .collect::<Vec<_>>();
        let extra = actual_names
            .difference(&expected_names)
            .cloned()
            .collect::<Vec<_>>();
        return Err(BuildError::Validation(format!(
            "{name} field set differs: missing [{}], extra [{}]",
            missing.join(", "),
            extra.join(", ")
        )));
    }

    let mut intervals = Vec::with_capacity(expected.len());
    for (field, offset, size) in expected {
        let actual_field = actual
            .get(*field)
            .ok_or_else(|| BuildError::Validation(format!("{name} is missing field `{field}`")))?;
        if actual_field != &(*offset, *size) {
            return Err(BuildError::Validation(format!(
                "{name} field `{field}` must be at {offset} with size {size}, found {} with size {}",
                actual_field.0, actual_field.1
            )));
        }
        let end = offset
            .checked_add(*size)
            .ok_or_else(|| BuildError::Validation(format!("{name} field `{field}` overflows")))?;
        intervals.push((*offset, end, *field));
    }
    intervals.sort_unstable_by_key(|(start, _, _)| *start);
    let mut cursor = 0;
    for (start, end, field) in intervals {
        if start != cursor {
            return Err(BuildError::Validation(format!(
                "{name} field `{field}` starts at {start}, expected contiguous offset {cursor}"
            )));
        }
        cursor = end;
    }
    if cursor != expected_bytes {
        return Err(BuildError::Validation(format!(
            "{name} is {cursor} bytes, expected {expected_bytes}"
        )));
    }
    Ok(())
}

fn table_cells(line: &str) -> Vec<&str> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

fn validate_error_codes(guardian: &str) -> BuildResult<()> {
    let source = section(guardian, "## 10. 错误目录", "## 11. 安全、测试与不包含范围")?;
    let mut actual = BTreeSet::new();
    for line in source.lines() {
        let cells = table_cells(line);
        let Some(code) = cells.first() else {
            continue;
        };
        if code.starts_with("IO") && !actual.insert((*code).to_owned()) {
            return Err(BuildError::Validation(format!(
                "Guardian error catalog repeats `{code}`"
            )));
        }
    }
    let expected = ERROR_CODES
        .split_ascii_whitespace()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if actual == expected {
        Ok(())
    } else {
        let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
        let extra = actual.difference(&expected).cloned().collect::<Vec<_>>();
        Err(BuildError::Validation(format!(
            "Guardian error catalog differs: missing [{}], extra [{}]",
            missing.join(", "),
            extra.join(", ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUARDIAN: &str = include_str!("../../../../Contracts/io/v1/guardian-contract.md");
    const PROTOCOL: &str = include_str!("../../../../Contracts/io/v1/protocol-matrix.md");
    const TARGET_PROFILE: &str = include_str!("../../../../Contracts/io/v1/target-profile.md");

    #[test]
    fn repository_specifications_are_complete() {
        assert!(validate_sources(GUARDIAN, PROTOCOL, TARGET_PROFILE).is_ok());
    }

    #[test]
    fn shifted_atomic_field_is_rejected() {
        let changed = GUARDIAN.replace(
            "| 160 | 8 | InputPublishToken |",
            "| 161 | 8 | InputPublishToken |",
        );
        let error = validate_sources(&changed, PROTOCOL, TARGET_PROFILE)
            .err()
            .map(|value| value.to_string());
        assert!(error.is_some_and(|value| value.contains("InputPublishToken")));
    }

    #[test]
    fn automatic_backend_switch_clause_is_required() {
        let changed = GUARDIAN.replace(
            "禁止由 jitter、单次 timeout 或 backend fault 自动选择下一后端",
            "允许自动选择下一后端",
        );
        let error = validate_sources(&changed, PROTOCOL, TARGET_PROFILE)
            .err()
            .map(|value| value.to_string());
        assert!(error.is_some_and(|value| value.contains("自动选择下一后端")));
    }

    #[test]
    fn protocol_role_cannot_disappear() {
        let changed = PROTOCOL.replace("Modbus TCP | Client", "Modbus TCP | Server");
        let error = validate_sources(GUARDIAN, &changed, TARGET_PROFILE)
            .err()
            .map(|value| value.to_string());
        assert!(error.is_some_and(|value| value.contains("Modbus TCP | Client")));
    }
}
