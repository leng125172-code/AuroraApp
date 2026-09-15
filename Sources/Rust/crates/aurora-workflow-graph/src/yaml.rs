use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::str;

use saphyr_parser::{Event, Parser, ScalarStyle, Span as ParserSpan, Tag};

use crate::diagnostic::{make_diagnostic, sort_diagnostics};
use crate::{SourceSpan, WorkflowDiagnostic, WorkflowDiagnosticCode, YamlSourceLimits};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum YamlValue {
    Null,
    Boolean(bool),
    Integer(i128),
    Float(String),
    String(String),
    Sequence(Rc<Vec<YamlNode>>),
    Mapping(Rc<Vec<MappingEntry>>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct YamlNode {
    pub(crate) value: YamlValue,
    pub(crate) span: SourceSpan,
    decoded_scalar_bytes: usize,
}

impl YamlNode {
    pub(crate) fn mapping(&self) -> Option<&[MappingEntry]> {
        match &self.value {
            YamlValue::Mapping(entries) => Some(entries.as_slice()),
            _ => None,
        }
    }

    pub(crate) fn sequence(&self) -> Option<&[Self]> {
        match &self.value {
            YamlValue::Sequence(items) => Some(items.as_slice()),
            _ => None,
        }
    }

    pub(crate) fn string(&self) -> Option<&str> {
        match &self.value {
            YamlValue::String(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) const fn integer(&self) -> Option<i128> {
        match self.value {
            YamlValue::Integer(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) const fn boolean(&self) -> Option<bool> {
        match self.value {
            YamlValue::Boolean(value) => Some(value),
            _ => None,
        }
    }

    fn with_span(mut self, span: SourceSpan) -> Self {
        self.span = span;
        match &mut self.value {
            YamlValue::Sequence(items) => {
                let expanded = items
                    .iter()
                    .cloned()
                    .map(|item| item.with_span(span))
                    .collect();
                *items = Rc::new(expanded);
            }
            YamlValue::Mapping(entries) => {
                let mut expanded = Vec::with_capacity(entries.len());
                for entry in entries.iter().cloned() {
                    let mut entry = entry;
                    entry.key_span = span;
                    entry.value = entry.value.with_span(span);
                    expanded.push(entry);
                }
                *entries = Rc::new(expanded);
            }
            _ => {}
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MappingEntry {
    pub(crate) key: String,
    pub(crate) key_span: SourceSpan,
    pub(crate) value: YamlNode,
}

pub(crate) struct YamlParseOutput {
    pub(crate) root: Option<YamlNode>,
    pub(crate) diagnostics: Vec<WorkflowDiagnostic>,
}

pub(crate) fn parse(
    source_path: &str,
    source_bytes: &[u8],
    limits: YamlSourceLimits,
) -> YamlParseOutput {
    if source_bytes.len() > limits.source_bytes() {
        return byte_failure(
            source_path,
            WorkflowDiagnosticCode::SourceLimitExceeded,
            limits.source_bytes(),
            source_bytes.len().min(u32::MAX as usize),
        );
    }
    let source = match str::from_utf8(source_bytes) {
        Ok(value) if !value.starts_with('\u{feff}') => value,
        Ok(value) => {
            return YamlParseOutput {
                root: None,
                diagnostics: vec![make_diagnostic(
                    source_path,
                    value,
                    WorkflowDiagnosticCode::InvalidEncoding,
                    SourceSpan::from_usize(0, 3.min(value.len())),
                    "",
                    None,
                )],
            };
        }
        Err(error) => {
            let start = error.valid_up_to();
            let end = error
                .error_len()
                .map_or(source_bytes.len(), |length| start.saturating_add(length));
            return byte_failure(
                source_path,
                WorkflowDiagnosticCode::InvalidEncoding,
                start,
                end,
            );
        }
    };

    Builder::new(source_path, source, limits).run()
}

fn byte_failure(
    source_path: &str,
    code: WorkflowDiagnosticCode,
    start: usize,
    end: usize,
) -> YamlParseOutput {
    YamlParseOutput {
        root: None,
        diagnostics: vec![make_diagnostic(
            source_path,
            "",
            code,
            SourceSpan::from_usize(start, end),
            "",
            None,
        )],
    }
}

enum FrameKind {
    Sequence(Vec<YamlNode>),
    Mapping {
        entries: Vec<MappingEntry>,
        keys: BTreeSet<String>,
        pending_key: Option<YamlNode>,
    },
}

struct Frame {
    kind: FrameKind,
    anchor: usize,
    start: SourceSpan,
}

struct Builder<'a> {
    source_path: &'a str,
    source: &'a str,
    limits: YamlSourceLimits,
    char_to_byte: Vec<usize>,
    frames: Vec<Frame>,
    anchors: BTreeMap<usize, YamlNode>,
    open_anchors: BTreeSet<usize>,
    root: Option<YamlNode>,
    diagnostics: Vec<WorkflowDiagnostic>,
    document_count: usize,
    aliases: usize,
    alias_expansion_nodes: usize,
    decoded_scalar_bytes: usize,
    stopped: bool,
}

impl<'a> Builder<'a> {
    fn new(source_path: &'a str, source: &'a str, limits: YamlSourceLimits) -> Self {
        let mut char_to_byte = source
            .char_indices()
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        char_to_byte.push(source.len());
        Self {
            source_path,
            source,
            limits,
            char_to_byte,
            frames: Vec::new(),
            anchors: BTreeMap::new(),
            open_anchors: BTreeSet::new(),
            root: None,
            diagnostics: Vec::new(),
            document_count: 0,
            aliases: 0,
            alias_expansion_nodes: 0,
            decoded_scalar_bytes: 0,
            stopped: false,
        }
    }

    fn run(mut self) -> YamlParseOutput {
        let mut parser = Parser::new_from_str(self.source);
        while !self.stopped {
            let Some(event) = parser.next() else {
                break;
            };
            match event {
                Ok((event, span)) => self.event(event, self.span(span)),
                Err(error) => {
                    let start = self.byte_offset(error.marker().index());
                    let end = self.next_byte_boundary(start);
                    self.push(
                        WorkflowDiagnosticCode::InvalidYaml,
                        SourceSpan::from_usize(start, end),
                    );
                    self.stopped = true;
                }
            }
        }
        if !self.stopped && (self.document_count != 1 || !self.frames.is_empty()) {
            self.push(WorkflowDiagnosticCode::InvalidYaml, SourceSpan::empty());
        }
        sort_diagnostics(&mut self.diagnostics);
        let root = self.diagnostics.is_empty().then_some(self.root).flatten();
        YamlParseOutput {
            root,
            diagnostics: self.diagnostics,
        }
    }

    fn event(&mut self, event: Event<'a>, span: SourceSpan) {
        match event {
            Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => {}
            Event::DocumentStart(_) => {
                self.document_count = self.document_count.saturating_add(1);
                if self.document_count > 1 {
                    self.push(WorkflowDiagnosticCode::InvalidYaml, span);
                    self.stopped = true;
                }
            }
            Event::Scalar(text, style, anchor, tag) => {
                self.add_scalar(text.as_ref(), style, anchor, tag.as_deref(), span);
            }
            Event::SequenceStart(anchor, tag) => {
                self.start_container(false, anchor, tag.as_deref(), span);
            }
            Event::MappingStart(anchor, tag) => {
                self.start_container(true, anchor, tag.as_deref(), span);
            }
            Event::SequenceEnd => self.end_container(false, span),
            Event::MappingEnd => self.end_container(true, span),
            Event::Alias(anchor) => self.add_alias(anchor, span),
            Event::Nothing => {
                self.push(WorkflowDiagnosticCode::InvalidYaml, span);
                self.stopped = true;
            }
        }
    }

    fn add_scalar(
        &mut self,
        text: &str,
        style: ScalarStyle,
        anchor: usize,
        tag: Option<&Tag>,
        span: SourceSpan,
    ) {
        if !self.add_scalar_bytes(text.len(), span) {
            return;
        }
        let value = match resolve_scalar(text, style, tag) {
            Ok(value) => value,
            Err(code) => {
                self.push(code, span);
                YamlValue::Null
            }
        };
        let node = YamlNode {
            value,
            span,
            decoded_scalar_bytes: text.len(),
        };
        if anchor != 0 {
            self.anchors.insert(anchor, node.clone());
        }
        self.add_node(node);
    }

    fn start_container(
        &mut self,
        mapping: bool,
        anchor: usize,
        tag: Option<&Tag>,
        span: SourceSpan,
    ) {
        if !container_tag_is_valid(mapping, tag) {
            self.push(WorkflowDiagnosticCode::UnsupportedYamlTag, span);
        }
        let next_depth = self.frames.len().saturating_add(1);
        if next_depth > self.limits.nesting_depth() {
            self.push(WorkflowDiagnosticCode::SourceLimitExceeded, span);
            self.stopped = true;
            return;
        }
        if anchor != 0 {
            self.open_anchors.insert(anchor);
        }
        let kind = if mapping {
            FrameKind::Mapping {
                entries: Vec::new(),
                keys: BTreeSet::new(),
                pending_key: None,
            }
        } else {
            FrameKind::Sequence(Vec::new())
        };
        self.frames.push(Frame {
            kind,
            anchor,
            start: span,
        });
    }

    fn end_container(&mut self, mapping: bool, end: SourceSpan) {
        let Some(frame) = self.frames.pop() else {
            self.push(WorkflowDiagnosticCode::InvalidYaml, end);
            self.stopped = true;
            return;
        };
        let (value, valid_kind) = match frame.kind {
            FrameKind::Sequence(items) => (YamlValue::Sequence(Rc::new(items)), !mapping),
            FrameKind::Mapping {
                entries,
                pending_key,
                ..
            } => {
                if pending_key.is_some() {
                    self.push(WorkflowDiagnosticCode::InvalidYaml, end);
                }
                (YamlValue::Mapping(Rc::new(entries)), mapping)
            }
        };
        if !valid_kind {
            self.push(WorkflowDiagnosticCode::InvalidYaml, end);
            self.stopped = true;
            return;
        }
        let span = SourceSpan {
            start: frame.start.start,
            end: end.end.max(frame.start.end),
        };
        let node = YamlNode {
            value,
            span,
            decoded_scalar_bytes: 0,
        };
        if frame.anchor != 0 {
            self.open_anchors.remove(&frame.anchor);
            self.anchors.insert(frame.anchor, node.clone());
        }
        self.add_node(node);
    }

    fn add_alias(&mut self, anchor: usize, span: SourceSpan) {
        self.aliases = self.aliases.saturating_add(1);
        if self.aliases > self.limits.aliases() {
            self.push(WorkflowDiagnosticCode::SourceLimitExceeded, span);
            self.stopped = true;
            return;
        }
        if self.open_anchors.contains(&anchor) {
            self.push(WorkflowDiagnosticCode::AliasCycle, span);
            return;
        }
        let Some(node) = self.anchors.get(&anchor) else {
            self.push(WorkflowDiagnosticCode::InvalidYaml, span);
            return;
        };
        let Some((nodes, scalar_bytes)) = measure(node) else {
            self.push(WorkflowDiagnosticCode::SourceLimitExceeded, span);
            self.stopped = true;
            return;
        };
        let Some(expanded) = self.alias_expansion_nodes.checked_add(nodes) else {
            self.push(WorkflowDiagnosticCode::SourceLimitExceeded, span);
            self.stopped = true;
            return;
        };
        if expanded > self.limits.alias_expansion_nodes() {
            self.push(WorkflowDiagnosticCode::SourceLimitExceeded, span);
            self.stopped = true;
            return;
        }
        if !self.add_scalar_bytes(scalar_bytes, span) {
            return;
        }
        self.alias_expansion_nodes = expanded;
        let Some(node) = self.anchors.get(&anchor).cloned() else {
            self.push(WorkflowDiagnosticCode::InvalidYaml, span);
            return;
        };
        self.add_node(node.with_span(span));
    }

    fn add_scalar_bytes(&mut self, count: usize, span: SourceSpan) -> bool {
        let Some(total) = self.decoded_scalar_bytes.checked_add(count) else {
            self.push(WorkflowDiagnosticCode::SourceLimitExceeded, span);
            self.stopped = true;
            return false;
        };
        if total > self.limits.decoded_scalar_bytes() {
            self.push(WorkflowDiagnosticCode::SourceLimitExceeded, span);
            self.stopped = true;
            return false;
        }
        self.decoded_scalar_bytes = total;
        true
    }

    fn add_node(&mut self, node: YamlNode) {
        let Some(frame) = self.frames.last_mut() else {
            if self.root.replace(node).is_some() {
                self.push(WorkflowDiagnosticCode::InvalidYaml, SourceSpan::empty());
                self.stopped = true;
            }
            return;
        };
        match &mut frame.kind {
            FrameKind::Sequence(items) => items.push(node),
            FrameKind::Mapping {
                entries,
                keys,
                pending_key,
            } => {
                if pending_key.is_none() {
                    *pending_key = Some(node);
                    return;
                }
                let Some(key_node) = pending_key.take() else {
                    return;
                };
                let Some(key) = key_node.string().filter(|key| !key.is_empty()) else {
                    self.push(WorkflowDiagnosticCode::InvalidField, key_node.span);
                    return;
                };
                if key == "<<" {
                    self.push(WorkflowDiagnosticCode::UnsupportedYamlTag, key_node.span);
                    return;
                }
                if !keys.insert(key.to_owned()) {
                    self.push(WorkflowDiagnosticCode::DuplicateMappingKey, key_node.span);
                    return;
                }
                entries.push(MappingEntry {
                    key: key.to_owned(),
                    key_span: key_node.span,
                    value: node,
                });
            }
        }
    }

    fn push(&mut self, code: WorkflowDiagnosticCode, span: SourceSpan) {
        self.diagnostics.push(make_diagnostic(
            self.source_path,
            self.source,
            code,
            span,
            "",
            None,
        ));
    }

    fn span(&self, span: ParserSpan) -> SourceSpan {
        SourceSpan::from_usize(
            self.byte_offset(span.start.index()),
            self.byte_offset(span.end.index()),
        )
    }

    fn byte_offset(&self, character_index: usize) -> usize {
        self.char_to_byte
            .get(character_index)
            .copied()
            .unwrap_or(self.source.len())
    }

    fn next_byte_boundary(&self, byte_offset: usize) -> usize {
        self.source
            .get(byte_offset..)
            .and_then(|tail| tail.chars().next())
            .map_or(byte_offset, |value| byte_offset + value.len_utf8())
    }
}

fn container_tag_is_valid(mapping: bool, tag: Option<&Tag>) -> bool {
    let Some(tag) = tag else {
        return true;
    };
    tag.is_yaml_core_schema()
        && matches!(
            (mapping, tag.suffix.as_str()),
            (true, "map") | (false, "seq")
        )
}

fn resolve_scalar(
    text: &str,
    style: ScalarStyle,
    tag: Option<&Tag>,
) -> Result<YamlValue, WorkflowDiagnosticCode> {
    if let Some(tag) = tag {
        if !tag.is_yaml_core_schema() {
            return Err(WorkflowDiagnosticCode::UnsupportedYamlTag);
        }
        return match tag.suffix.as_str() {
            "str" => Ok(YamlValue::String(text.to_owned())),
            "null" if matches!(text, "~" | "null" | "Null" | "NULL" | "") => Ok(YamlValue::Null),
            "bool" => parse_boolean(text)
                .map(YamlValue::Boolean)
                .ok_or(WorkflowDiagnosticCode::InvalidYaml),
            "int" => parse_integer(text)
                .map(YamlValue::Integer)
                .ok_or(WorkflowDiagnosticCode::InvalidYaml),
            "float" => parse_float(text),
            "map" | "seq" => Err(WorkflowDiagnosticCode::InvalidYaml),
            _ => Err(WorkflowDiagnosticCode::UnsupportedYamlTag),
        };
    }
    if style != ScalarStyle::Plain {
        return Ok(YamlValue::String(text.to_owned()));
    }
    if matches!(
        text,
        ".inf"
            | ".Inf"
            | ".INF"
            | "+.inf"
            | "+.Inf"
            | "+.INF"
            | "-.inf"
            | "-.Inf"
            | "-.INF"
            | ".nan"
            | ".NaN"
            | ".NAN"
            | "NaN"
            | "NAN"
            | "Infinity"
            | "INFINITY"
    ) {
        return Err(WorkflowDiagnosticCode::InvalidYaml);
    }
    if matches!(text, "" | "~" | "null" | "Null" | "NULL") {
        return Ok(YamlValue::Null);
    }
    if let Some(value) = parse_boolean(text) {
        return Ok(YamlValue::Boolean(value));
    }
    if let Some(value) = parse_integer(text) {
        return Ok(YamlValue::Integer(value));
    }
    if let Some(value) = parse_implicit_float(text) {
        return Ok(YamlValue::Float(value));
    }
    Ok(YamlValue::String(text.to_owned()))
}

fn parse_boolean(text: &str) -> Option<bool> {
    match text {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

fn parse_integer(text: &str) -> Option<i128> {
    let (negative, unsigned) = if let Some(rest) = text.strip_prefix('-') {
        (true, rest)
    } else {
        (false, text.strip_prefix('+').unwrap_or(text))
    };
    if unsigned.is_empty() {
        return None;
    }
    let (digits, radix) = unsigned.strip_prefix("0x").map_or_else(
        || {
            unsigned
                .strip_prefix("0o")
                .map_or((unsigned, 10), |value| (value, 8))
        },
        |value| (value, 16),
    );
    let first_is_digit = digits.bytes().next().is_some_and(|byte| match radix {
        8 => matches!(byte, b'0'..=b'7'),
        10 => byte.is_ascii_digit(),
        16 => byte.is_ascii_hexdigit(),
        _ => false,
    });
    if digits.is_empty()
        || !first_is_digit
        || !digits.bytes().all(|byte| match radix {
            8 => matches!(byte, b'0'..=b'7'),
            10 => byte.is_ascii_digit(),
            16 => byte.is_ascii_hexdigit(),
            _ => false,
        } || byte == b'_')
        || !digits.bytes().any(|byte| byte != b'_')
    {
        return None;
    }
    let normalized = digits
        .bytes()
        .filter(|byte| *byte != b'_')
        .map(char::from)
        .collect::<String>();
    let magnitude = i128::from_str_radix(&normalized, radix).ok()?;
    if negative {
        magnitude.checked_neg()
    } else {
        Some(magnitude)
    }
}

fn parse_implicit_float(text: &str) -> Option<String> {
    let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
    let (mantissa, exponent) = unsigned
        .split_once(['e', 'E'])
        .map_or((unsigned, None), |(mantissa, exponent)| {
            (mantissa, Some(exponent))
        });
    if exponent.is_some_and(|value| value.contains(['e', 'E'])) {
        return None;
    }
    let exponent_valid = exponent.is_none_or(|value| {
        let digits = value.strip_prefix(['-', '+']).unwrap_or(value);
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    });
    let mantissa_valid = if let Some(fraction) = mantissa.strip_prefix('.') {
        !fraction.is_empty()
            && fraction
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'_')
            && fraction.bytes().any(|byte| byte.is_ascii_digit())
    } else {
        let mut dots = 0_u8;
        mantissa.bytes().enumerate().all(|(index, byte)| {
            if byte == b'.' {
                dots = dots.saturating_add(1);
                dots == 1
            } else {
                (index == 0 && byte.is_ascii_digit())
                    || (index > 0 && (byte.is_ascii_digit() || byte == b'_'))
            }
        })
    };
    if !exponent_valid || !mantissa_valid || (!mantissa.contains('.') && exponent.is_none()) {
        return None;
    }
    let normalized = text.replace('_', "");
    normalized
        .parse::<f64>()
        .ok()?
        .is_finite()
        .then_some(text.to_owned())
}

fn parse_float(text: &str) -> Result<YamlValue, WorkflowDiagnosticCode> {
    let without_separators = text.replace('_', "");
    let normalized = if let Some(rest) = without_separators.strip_prefix("+.") {
        format!("+0.{rest}")
    } else if let Some(rest) = without_separators.strip_prefix("-.") {
        format!("-0.{rest}")
    } else if let Some(rest) = without_separators.strip_prefix('.') {
        format!("0.{rest}")
    } else if without_separators.ends_with('.') {
        format!("{without_separators}0")
    } else {
        without_separators
    };
    let value = normalized
        .parse::<f64>()
        .map_err(|_| WorkflowDiagnosticCode::InvalidYaml)?;
    if value.is_finite() {
        Ok(YamlValue::Float(text.to_owned()))
    } else {
        Err(WorkflowDiagnosticCode::InvalidYaml)
    }
}

fn measure(node: &YamlNode) -> Option<(usize, usize)> {
    match &node.value {
        YamlValue::Null
        | YamlValue::Boolean(_)
        | YamlValue::Integer(_)
        | YamlValue::Float(_)
        | YamlValue::String(_) => Some((1, node.decoded_scalar_bytes)),
        YamlValue::Sequence(items) => {
            let mut nodes = 1_usize;
            let mut bytes = 0_usize;
            for item in items.iter() {
                let (item_nodes, item_bytes) = measure(item)?;
                nodes = nodes.checked_add(item_nodes)?;
                bytes = bytes.checked_add(item_bytes)?;
            }
            Some((nodes, bytes))
        }
        YamlValue::Mapping(entries) => {
            let mut nodes = 1_usize;
            let mut bytes = 0_usize;
            for entry in entries.iter() {
                nodes = nodes.checked_add(1)?;
                bytes = bytes.checked_add(entry.key.len())?;
                let (value_nodes, value_bytes) = measure(&entry.value)?;
                nodes = nodes.checked_add(value_nodes)?;
                bytes = bytes.checked_add(value_bytes)?;
            }
            Some((nodes, bytes))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(
        source_bytes: usize,
        nesting_depth: usize,
        aliases: usize,
        alias_expansion_nodes: usize,
        decoded_scalar_bytes: usize,
    ) -> Option<YamlSourceLimits> {
        YamlSourceLimits::new(
            source_bytes,
            nesting_depth,
            aliases,
            alias_expansion_nodes,
            decoded_scalar_bytes,
        )
        .ok()
    }

    fn assert_single_code(source: &[u8], limits: YamlSourceLimits, code: WorkflowDiagnosticCode) {
        let output = parse("test.yaml", source, limits);
        assert!(output.root.is_none());
        assert_eq!(output.diagnostics.len(), 1, "{:?}", output.diagnostics);
        assert_eq!(output.diagnostics[0].code, code);
    }

    #[test]
    fn encoding_tag_merge_and_document_rejections_are_exact() {
        let result = limits(256, 8, 8, 32, 256);
        assert!(result.is_some());
        let Some(limits) = result else {
            return;
        };
        assert_single_code(
            b"\xef\xbb\xbfkey: value\n",
            limits,
            WorkflowDiagnosticCode::InvalidEncoding,
        );
        assert_single_code(
            &[0xff, 0xfe],
            limits,
            WorkflowDiagnosticCode::InvalidEncoding,
        );
        assert_single_code(
            b"key: !aurora value\n",
            limits,
            WorkflowDiagnosticCode::UnsupportedYamlTag,
        );
        assert_single_code(
            b"base: &base { key: value }\ncopy: { <<: *base }\n",
            limits,
            WorkflowDiagnosticCode::UnsupportedYamlTag,
        );
        assert_single_code(
            b"---\na: b\n---\nc: d\n",
            limits,
            WorkflowDiagnosticCode::InvalidYaml,
        );
    }

    #[test]
    fn every_yaml_budget_accepts_equality_and_rejects_first_excess_once() {
        let source = b"value: &flag false\ncopy: *flag\n";
        let exact = limits(source.len(), 1, 1, 1, 19);
        assert!(exact.is_some());
        let Some(exact) = exact else {
            return;
        };
        let accepted = parse("test.yaml", source, exact);
        assert!(
            accepted.diagnostics.is_empty(),
            "{:?}",
            accepted.diagnostics
        );
        assert!(accepted.root.is_some());

        let cases = [
            limits(source.len() - 1, 1, 1, 1, 19),
            limits(source.len(), 1, 1, 1, 18),
            limits(source.len(), 1, 1, 1, 14),
        ];
        for limits in cases {
            assert!(limits.is_some());
            let Some(limits) = limits else {
                return;
            };
            assert_single_code(source, limits, WorkflowDiagnosticCode::SourceLimitExceeded);
        }

        let nested = b"- [value]\n";
        let result = limits(nested.len(), 1, 1, 1, 16);
        assert!(result.is_some());
        let Some(shallow) = result else {
            return;
        };
        assert_single_code(nested, shallow, WorkflowDiagnosticCode::SourceLimitExceeded);

        let two_aliases = b"value: &flag false\none: *flag\ntwo: *flag\n";
        let result = limits(two_aliases.len(), 2, 1, 2, 64);
        assert!(result.is_some());
        let Some(one_alias) = result else {
            return;
        };
        assert_single_code(
            two_aliases,
            one_alias,
            WorkflowDiagnosticCode::SourceLimitExceeded,
        );
    }

    #[test]
    fn core_numeric_resolution_does_not_capture_plain_strings() {
        let source = b"integer: 1_024\nfloat: 1_0.5_0\ntext: version1.2\nunderscore: _1\n";
        let result = limits(source.len(), 2, 1, 1, 128);
        assert!(result.is_some());
        let Some(limits) = result else {
            return;
        };
        let output = parse("test.yaml", source, limits);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let Some(root) = output.root else {
            return;
        };
        let Some(mapping) = root.mapping() else {
            return;
        };
        assert_eq!(mapping[0].value.integer(), Some(1_024));
        assert!(matches!(mapping[1].value.value, YamlValue::Float(_)));
        assert_eq!(mapping[2].value.string(), Some("version1.2"));
        assert_eq!(mapping[3].value.string(), Some("_1"));
    }

    #[test]
    fn unicode_diagnostic_spans_use_utf8_byte_offsets() {
        let source = "名字: first\n名字: second\n";
        let result = limits(source.len(), 2, 1, 1, 64);
        assert!(result.is_some());
        let Some(limits) = result else {
            return;
        };
        let output = parse("test.yaml", source.as_bytes(), limits);
        assert_eq!(output.diagnostics.len(), 1, "{:?}", output.diagnostics);
        assert_eq!(
            output.diagnostics[0].code,
            WorkflowDiagnosticCode::DuplicateMappingKey
        );
        assert_eq!(output.diagnostics[0].span.start, 14);
        assert_eq!(output.diagnostics[0].start.line, 2);
        assert_eq!(output.diagnostics[0].start.column, 1);
    }
}
