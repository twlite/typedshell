//! Target backends for typed-shell IR.

pub mod bash;
pub mod powershell;

use crate::{compiler::diagnostics::CompileError, ir::Program};

/// A backend turns target-independent, typed IR into a standalone program.
pub trait Backend {
    fn emit(&self, program: &Program) -> Result<String, CompileError>;
}

/// Split a TypeScript comment into shell comment bodies (the `# ` prefix is
/// added by the caller).
///
/// - `// text` yields `text` with one optional leading space removed.
/// - `/* ... */` blocks additionally drop the per-line JSDoc `*` markers
///   (plus one following space or tab), so `/** ... */` renders as plain
///   text instead of leaking `*` prefixes into the generated script.
/// - Blank lines inside block comments are dropped; a comment with no
///   remaining content yields no lines.
pub fn shell_comment_lines(comment: &str) -> Vec<String> {
    if let Some(line) = comment.strip_prefix("//") {
        let text = line.strip_prefix(' ').unwrap_or(line);
        return vec![text.to_owned()];
    }
    if let Some(block) = comment
        .strip_prefix("/*")
        .and_then(|text| text.strip_suffix("*/"))
    {
        let mut lines = Vec::new();
        for line in block.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let mut text = line.trim_start();
            if let Some(rest) = text.strip_prefix('*') {
                text = rest
                    .strip_prefix(' ')
                    .or_else(|| rest.strip_prefix('\t'))
                    .unwrap_or(rest);
            }
            let text = text.trim_end();
            if !text.is_empty() {
                lines.push(text.to_owned());
            }
        }
        return lines;
    }
    vec![comment.to_owned()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsdoc_blocks_render_as_plain_text() {
        assert_eq!(
            shell_comment_lines(
                "/**\n   * Generates and stores the next value\n   * @returns current value\n   */"
            ),
            vec![
                "Generates and stores the next value".to_owned(),
                "@returns current value".to_owned(),
            ]
        );
        assert_eq!(
            shell_comment_lines("/** documentation */"),
            vec!["documentation".to_owned()]
        );
        assert_eq!(
            shell_comment_lines("// ordinary"),
            vec!["ordinary".to_owned()]
        );
        assert_eq!(shell_comment_lines("/**/"), Vec::<String>::new());
    }
}
