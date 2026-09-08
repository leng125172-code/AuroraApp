//! Aurora ST 规范源的 host-only 完整性门禁。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{BuildError, BuildResult};

const GRAMMAR_PATH: &str = "Sources/Contracts/st/v1/aurora-st.ebnf";
const LANGUAGE_PATH: &str = "Sources/Contracts/st/v1/language.md";
const ADDRESS_MAPPING_PATH: &str = "Sources/Contracts/st/v1/address-mapping.md";

const REQUIRED_GRAMMAR_RULES: &str = "
compilation_unit version_directive type_block type_declaration type_specification elementary_type
string_type array_type structure_type structure_field enumeration_type enumeration_item
global_variable_block global_variable_declaration function_declaration function_block_declaration
program_declaration function_variable_block stateful_variable_block input_variable_block
output_variable_block local_variable_block temporary_variable_block variable_declaration
identifier_list statement_list statement assignment_statement function_block_call_statement
function_block_argument if_statement for_statement return_statement expression or_else_expression
xor_expression or_expression and_then_expression and_expression comparison_expression
comparison_operator additive_expression multiplicative_expression unary_expression
primary_expression call_expression assignable index_suffix field_suffix qualified_identifier literal
qualified_literal constant_expression integer_constant_expression direct_address direct_bit_address
direct_scalar_address address_area address_width bit_offset decimal_offset positive_decimal identifier
boolean_literal integer_literal real_literal string_literal wstring_literal decimal_digit nonzero_digit
end_of_file
";

/// 验证规范文件存在、文法闭合，并且冻结诊断没有重复、遗漏或悬空引用。
pub(crate) fn validate(repository_root: &Path) -> BuildResult<()> {
    let grammar = read(repository_root.join(GRAMMAR_PATH))?;
    let language = read(repository_root.join(LANGUAGE_PATH))?;
    let address_mapping = read(repository_root.join(ADDRESS_MAPPING_PATH))?;
    validate_sources(&grammar, &language, &address_mapping)
}

fn read(path: PathBuf) -> BuildResult<String> {
    fs::read_to_string(&path).map_err(|source| BuildError::Io {
        operation: "read Aurora ST specification",
        path,
        source,
    })
}

fn validate_sources(grammar: &str, language: &str, address_mapping: &str) -> BuildResult<()> {
    validate_grammar(grammar)?;
    validate_diagnostics(language, address_mapping)
}

fn validate_grammar(grammar: &str) -> BuildResult<()> {
    let definitions = grammar_definitions(grammar)?;
    let required = REQUIRED_GRAMMAR_RULES
        .split_ascii_whitespace()
        .collect::<BTreeSet<_>>();
    if definitions != required {
        let missing = required
            .difference(&definitions)
            .copied()
            .collect::<Vec<_>>();
        let extra = definitions
            .difference(&required)
            .copied()
            .collect::<Vec<_>>();
        return Err(BuildError::Validation(format!(
            "Aurora ST Preview 1.0 grammar rule set differs: missing [{}], extra [{}]",
            missing.join(", "),
            extra.join(", ")
        )));
    }

    let references = grammar_references(grammar)?;
    let unknown = references
        .difference(&definitions)
        .copied()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(BuildError::Validation(format!(
            "Aurora ST Preview 1.0 grammar has undefined rules: {}",
            unknown.join(", ")
        )));
    }
    if !grammar.contains("version_directive = \"AURORA_ST\", \"VERSION\", \"1.0\", \";\" ;")
        || !grammar
            .contains("global_variable_declaration = identifier, \"AT\", direct_address, \":\"")
    {
        return Err(BuildError::Validation(
            "Aurora ST Preview 1.0 grammar lost its exact version or mandatory AT boundary"
                .to_owned(),
        ));
    }
    Ok(())
}

fn grammar_definitions(grammar: &str) -> BuildResult<BTreeSet<&str>> {
    let mut definitions = BTreeSet::new();
    for line in grammar.lines() {
        let Some((name, _right)) = line.split_once(" = ") else {
            continue;
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        {
            continue;
        }
        if !definitions.insert(name) {
            return Err(BuildError::Validation(format!(
                "Aurora ST grammar defines `{name}` more than once"
            )));
        }
    }
    Ok(definitions)
}

fn grammar_references(grammar: &str) -> BuildResult<BTreeSet<&str>> {
    let bytes = grammar.as_bytes();
    let mut references = BTreeSet::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'(' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_delimited(bytes, index + 2, b'*', b')', "comment")?;
            }
            b'"' => {
                index = skip_delimited(bytes, index + 1, b'"', 0, "terminal")?;
            }
            b'?' => {
                index = skip_delimited(bytes, index + 1, b'?', 0, "special sequence")?;
            }
            byte if byte.is_ascii_lowercase() => {
                let start = index;
                index += 1;
                while bytes
                    .get(index)
                    .is_some_and(|value| value.is_ascii_lowercase() || *value == b'_')
                {
                    index += 1;
                }
                let word = &grammar[start..index];
                references.insert(word);
            }
            _ => index += 1,
        }
    }
    Ok(references)
}

fn skip_delimited(
    bytes: &[u8],
    mut index: usize,
    closing_first: u8,
    closing_second: u8,
    kind: &str,
) -> BuildResult<usize> {
    while index < bytes.len() {
        if bytes[index] == closing_first
            && (closing_second == 0 || bytes.get(index + 1) == Some(&closing_second))
        {
            return Ok(index + usize::from(closing_second != 0) + 1);
        }
        index += 1;
    }
    Err(BuildError::Validation(format!(
        "Aurora ST grammar has an unterminated {kind}"
    )))
}

fn validate_diagnostics(language: &str, address_mapping: &str) -> BuildResult<()> {
    let mut definitions = BTreeSet::new();
    for source in [language, address_mapping] {
        for line in source.lines() {
            let Some(rest) = line.strip_prefix("| `") else {
                continue;
            };
            let Some((code, _rest)) = rest.split_once('`') else {
                continue;
            };
            if is_diagnostic_code(code) && !definitions.insert(code.to_owned()) {
                return Err(BuildError::Validation(format!(
                    "Aurora ST diagnostic `{code}` is defined more than once"
                )));
            }
        }
    }

    let required = required_diagnostic_codes();
    if definitions != required {
        let missing = required
            .difference(&definitions)
            .cloned()
            .collect::<Vec<_>>();
        let extra = definitions
            .difference(&required)
            .cloned()
            .collect::<Vec<_>>();
        return Err(BuildError::Validation(format!(
            "Aurora ST Preview 1.0 diagnostic catalog differs: missing [{}], extra [{}]",
            missing.join(", "),
            extra.join(", ")
        )));
    }

    for code in diagnostic_references(language).chain(diagnostic_references(address_mapping)) {
        if !definitions.contains(code) {
            return Err(BuildError::Validation(format!(
                "Aurora ST specification references undefined diagnostic `{code}`"
            )));
        }
    }
    Ok(())
}

fn required_diagnostic_codes() -> BTreeSet<String> {
    let mut codes = BTreeSet::new();
    for (start, end) in [
        (1, 7),
        (101, 103),
        (1001, 1006),
        (2001, 2008),
        (3001, 3005),
        (4001, 4004),
        (5001, 5021),
    ] {
        for value in start..=end {
            codes.insert(format!("ST{value:04}"));
        }
    }
    for value in 1..=6 {
        codes.insert(format!("STF{value:04}"));
    }
    codes
}

fn diagnostic_references(source: &str) -> impl Iterator<Item = &str> {
    source.match_indices("ST").filter_map(|(start, _)| {
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
        .strip_prefix("STF")
        .or_else(|| value.strip_prefix("ST"));
    digits
        .is_some_and(|digits| digits.len() == 4 && digits.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::validate_sources;

    const GRAMMAR: &str = include_str!("../../../../Contracts/st/v1/aurora-st.ebnf");
    const LANGUAGE: &str = include_str!("../../../../Contracts/st/v1/language.md");
    const ADDRESS_MAPPING: &str = include_str!("../../../../Contracts/st/v1/address-mapping.md");

    #[test]
    fn checked_in_st_specs_are_complete_and_unambiguous() {
        assert!(validate_sources(GRAMMAR, LANGUAGE, ADDRESS_MAPPING).is_ok());
    }

    #[test]
    fn undefined_grammar_rule_is_rejected() {
        let broken = GRAMMAR.replace(
            "statement_list = { statement } ;",
            "statement_list = { missing_rule } ;",
        );
        assert!(validate_sources(&broken, LANGUAGE, ADDRESS_MAPPING).is_err());
    }

    #[test]
    fn duplicate_or_missing_diagnostic_is_rejected() {
        let broken = LANGUAGE.replace(
            "| `ST4004` | NonFiniteConstant |",
            "| `ST4003` | NonFiniteConstant |",
        );
        assert!(validate_sources(GRAMMAR, &broken, ADDRESS_MAPPING).is_err());
    }
}
