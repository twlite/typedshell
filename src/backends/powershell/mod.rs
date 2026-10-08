//! PowerShell 7.4+ code generation for the typed IR.

pub mod emitter;

use crate::{backends::Backend, compiler::diagnostics::CompileError, ir::Program};

/// The PowerShell 7.4 backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct PowerShell;

impl Backend for PowerShell {
    fn emit(&self, program: &Program) -> Result<String, CompileError> {
        emitter::emit_program(program)
    }
}

/// Quote a static string as a single-quoted PowerShell literal.
///
/// Single-quoted strings are literal: only `'` needs escaping (by doubling).
/// Newlines, tabs, `$`, backticks, and double quotes pass through unchanged.
pub fn ps_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    let mut output = String::with_capacity(value.len() + 2);
    output.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            output.push_str("''");
        } else {
            output.push(ch);
        }
    }
    output.push('\'');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_quotes_are_doubled_and_everything_else_is_literal() {
        assert_eq!(ps_quote(""), "''");
        assert_eq!(ps_quote("hello"), "'hello'");
        assert_eq!(ps_quote("it's"), "'it''s'");
        assert_eq!(
            ps_quote("a $HOME `tick` \"q\" \\ end"),
            "'a $HOME `tick` \"q\" \\ end'"
        );
        assert_eq!(ps_quote("line\nbreak"), "'line\nbreak'");
    }
}
