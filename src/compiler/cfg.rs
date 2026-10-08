//! Source-position-preserving conditional compilation.
//!
//! The directives are comments in the source language, so this pass runs before
//! parsing. It only recognizes standalone line comments outside strings,
//! templates, and block comments.

use crate::{compiler::diagnostics::CompileError, options::CompileOptions};
use std::path::Path;

#[derive(Debug, Clone, Copy)]
struct Comment {
    start: usize,
    end: usize,
    content_start: usize,
    content_end: usize,
    standalone: bool,
}

#[derive(Debug, Clone)]
struct Conditional {
    parent_active: bool,
    active: bool,
    any_taken: bool,
    saw_else: bool,
    start: usize,
}

/// Apply `#if` / `#elif` / `#else` / `#endif` and `@cfg(...)` directives.
///
/// Removed text is replaced with ASCII spaces while CR and LF bytes remain in
/// place. The result therefore has exactly the same byte offsets as `source`.
pub fn preprocess(
    path: &Path,
    source: &str,
    options: &CompileOptions,
) -> Result<String, CompileError> {
    let comments = line_comments(source);
    // Byte offsets are the canonical positions; a map indexed by the line's
    // start avoids repeatedly searching all comments while preserving CRLF.
    let mut standalone = std::collections::HashMap::new();
    for comment in comments.into_iter().filter(|comment| comment.standalone) {
        let line_start = source[..comment.start]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        standalone.insert(line_start, comment);
    }

    let mut output = source.as_bytes().to_vec();
    let mut stack: Vec<Conditional> = Vec::new();
    let mut active = true;
    let mut line_start = 0;
    while line_start < source.len() {
        let content_end = source[line_start..]
            .find('\n')
            .map_or(source.len(), |offset| line_start + offset);
        let line_end = if content_end > line_start && source.as_bytes()[content_end - 1] == b'\r' {
            content_end - 1
        } else {
            content_end
        };

        if let Some(comment) = standalone.get(&line_start).copied() {
            let text = source[comment.content_start..comment.content_end].trim();
            if let Some((directive, argument)) = conditional_directive(text) {
                match directive {
                    "if" => {
                        let value =
                            eval_predicate(path, source, comment.content_start, argument, options)?;
                        let frame = Conditional {
                            parent_active: active,
                            active: active && value,
                            any_taken: value,
                            saw_else: false,
                            start: comment.start,
                        };
                        active = frame.active;
                        stack.push(frame);
                    }
                    "elif" => {
                        let Some(frame) = stack.last_mut() else {
                            return Err(error(
                                path,
                                source,
                                comment.start,
                                comment.end - comment.start,
                                "`#elif` without a matching `#if`",
                            ));
                        };
                        if frame.saw_else {
                            return Err(error(
                                path,
                                source,
                                comment.start,
                                comment.end - comment.start,
                                "`#elif` cannot follow `#else`",
                            ));
                        }
                        let value =
                            eval_predicate(path, source, comment.content_start, argument, options)?;
                        frame.active = frame.parent_active && !frame.any_taken && value;
                        frame.any_taken |= value;
                        active = frame.active;
                    }
                    "else" => {
                        let Some(frame) = stack.last_mut() else {
                            return Err(error(
                                path,
                                source,
                                comment.start,
                                comment.end - comment.start,
                                "`#else` without a matching `#if`",
                            ));
                        };
                        if !argument.trim().is_empty() {
                            return Err(error(
                                path,
                                source,
                                comment.start,
                                comment.end - comment.start,
                                "`#else` does not take a predicate",
                            ));
                        }
                        if frame.saw_else {
                            return Err(error(
                                path,
                                source,
                                comment.start,
                                comment.end - comment.start,
                                "duplicate `#else`",
                            ));
                        }
                        frame.saw_else = true;
                        frame.active = frame.parent_active && !frame.any_taken;
                        frame.any_taken = true;
                        active = frame.active;
                    }
                    "endif" => {
                        if !argument.trim().is_empty() {
                            return Err(error(
                                path,
                                source,
                                comment.start,
                                comment.end - comment.start,
                                "`#endif` does not take a predicate",
                            ));
                        }
                        if stack.pop().is_none() {
                            return Err(error(
                                path,
                                source,
                                comment.start,
                                comment.end - comment.start,
                                "`#endif` without a matching `#if`",
                            ));
                        }
                        active = stack.last().is_none_or(|frame| frame.active);
                    }
                    _ => unreachable!(),
                }
                blank(&mut output, line_start, line_end);
            } else if let Some(after_name) = text.strip_prefix("@cfg") {
                if !after_name.is_empty()
                    && !after_name.starts_with('(')
                    && !after_name.chars().next().is_some_and(char::is_whitespace)
                {
                    if !active {
                        blank(&mut output, line_start, line_end);
                    }
                    line_start = next_line_start(content_end, source);
                    continue;
                }
                let Some(argument) = cfg_directive(text) else {
                    return Err(error(
                        path,
                        source,
                        comment.start,
                        comment.end - comment.start,
                        "expected `@cfg(predicate)`",
                    ));
                };
                let include =
                    eval_predicate(path, source, comment.content_start, argument, options)?;
                blank(&mut output, line_start, line_end);
                if !include {
                    let next =
                        skip_space_and_comments(source, next_line_start(content_end, source));
                    if next < source.len()
                        && !starts_preprocessor_directive(source, next)
                        && let Some(end) = statement_end(source, next)
                    {
                        blank(&mut output, next, end);
                    }
                }
            } else if !active {
                blank(&mut output, line_start, line_end);
            }
        } else if !active {
            blank(&mut output, line_start, line_end);
        }

        line_start = next_line_start(content_end, source);
    }

    if let Some(frame) = stack.last() {
        return Err(error(
            path,
            source,
            frame.start,
            2,
            "unterminated `#if` block",
        ));
    }
    // `String::from_utf8` is infallible here: every byte in a removed UTF-8
    // sequence is replaced, so no partial multibyte sequence survives.
    Ok(String::from_utf8(output).expect("blanking source preserves valid UTF-8"))
}

fn next_line_start(line_end: usize, source: &str) -> usize {
    if line_end < source.len() && source.as_bytes()[line_end] == b'\n' {
        line_end + 1
    } else {
        line_end
    }
}

fn blank(bytes: &mut [u8], start: usize, end: usize) {
    for byte in bytes.get_mut(start..end).into_iter().flatten() {
        if *byte != b'\n' && *byte != b'\r' {
            *byte = b' ';
        }
    }
}

fn error(
    path: &Path,
    source: &str,
    start: usize,
    len: usize,
    message: impl Into<String>,
) -> CompileError {
    CompileError::new(path.display().to_string(), source, start, len, message)
}

fn conditional_directive(text: &str) -> Option<(&'static str, &str)> {
    for (word, name) in [
        ("#if", "if"),
        ("#elif", "elif"),
        ("#else", "else"),
        ("#endif", "endif"),
    ] {
        if let Some(rest) = text.strip_prefix(word)
            && (rest.is_empty() || rest.chars().next().is_some_and(char::is_whitespace))
        {
            return Some((name, rest.trim_start()));
        }
    }
    None
}

fn cfg_directive(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("@cfg")?;
    if rest.is_empty()
        || !(rest.starts_with('(') || rest.chars().next().is_some_and(char::is_whitespace))
    {
        return None;
    }
    let rest = rest.trim_start();
    let body = rest.strip_prefix('(')?;
    let close = body.rfind(')')?;
    if !body[close + 1..].trim().is_empty() {
        return None;
    }
    Some(&body[..close])
}

/// Collect `//` comments while ignoring all quoted/template contents and block
/// comments. The entire template literal, including `${...}`, is intentionally
/// opaque to directives: template contents are source text, not directives.
fn line_comments(source: &str) -> Vec<Comment> {
    let bytes = source.as_bytes();
    let mut comments = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = (index + 2).min(bytes.len());
                    } else if bytes[index] == quote {
                        index += 1;
                        break;
                    } else {
                        index += 1;
                    }
                }
            }
            b'`' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = (index + 2).min(bytes.len());
                    } else if bytes[index] == b'`' {
                        index += 1;
                        break;
                    } else {
                        index += 1;
                    }
                }
            }
            b'/' if index + 1 < bytes.len() && bytes[index + 1] == b'/' => {
                let start = index;
                let line_start = source[..start].rfind('\n').map_or(0, |n| n + 1);
                let standalone = source[line_start..start].trim().is_empty();
                let content_start = start + 2;
                index = content_start;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                let end = index;
                let content_end = if end > content_start && bytes[end - 1] == b'\r' {
                    end - 1
                } else {
                    end
                };
                comments.push(Comment {
                    start,
                    end,
                    content_start,
                    content_end,
                    standalone,
                });
            }
            b'/' if index + 1 < bytes.len() && bytes[index + 1] == b'*' => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            _ => index += 1,
        }
    }
    comments
}

fn eval_predicate(
    path: &Path,
    source: &str,
    start: usize,
    expression: &str,
    options: &CompileOptions,
) -> Result<bool, CompileError> {
    eval_predicate_result(expression, options)
        .map(|(value, _)| value)
        .map_err(|(offset, message)| error(path, source, start + offset, 1, message))
}

fn eval_predicate_result(
    expression: &str,
    options: &CompileOptions,
) -> Result<(bool, (usize, String)), (usize, String)> {
    let mut parser = PredicateParser {
        input: expression.as_bytes(),
        index: 0,
        options,
    };
    let value = parser.parse_or()?;
    parser.whitespace();
    if parser.index < parser.input.len() {
        return Err((
            parser.index,
            "unexpected token in conditional predicate".into(),
        ));
    }
    Ok((value, (0, String::new())))
}

struct PredicateParser<'a> {
    input: &'a [u8],
    index: usize,
    options: &'a CompileOptions,
}

impl PredicateParser<'_> {
    fn whitespace(&mut self) {
        while self.index < self.input.len() && self.input[self.index].is_ascii_whitespace() {
            self.index += 1;
        }
    }

    fn eat(&mut self, token: &[u8]) -> bool {
        self.whitespace();
        if self.input.get(self.index..self.index + token.len()) == Some(token) {
            self.index += token.len();
            true
        } else {
            false
        }
    }

    fn parse_or(&mut self) -> Result<bool, (usize, String)> {
        let mut value = self.parse_and()?;
        loop {
            if self.eat(b"||") {
                let rhs = self.parse_and()?;
                value |= rhs;
            } else {
                break;
            }
        }
        Ok(value)
    }

    fn parse_and(&mut self) -> Result<bool, (usize, String)> {
        let mut value = self.parse_unary()?;
        loop {
            if self.eat(b"&&") {
                let rhs = self.parse_unary()?;
                value &= rhs;
            } else {
                break;
            }
        }
        Ok(value)
    }

    fn parse_unary(&mut self) -> Result<bool, (usize, String)> {
        if self.eat(b"!") {
            return Ok(!self.parse_unary()?);
        }
        if self.eat(b"(") {
            let value = self.parse_or()?;
            if !self.eat(b")") {
                return Err((self.index, "expected `)` in conditional predicate".into()));
            }
            return Ok(value);
        }
        self.parse_atom()
    }

    fn parse_atom(&mut self) -> Result<bool, (usize, String)> {
        self.whitespace();
        let start = self.index;
        let name = self
            .identifier()
            .ok_or_else(|| (start, "expected a platform predicate".into()))?;
        match name.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            "windows" => Ok(self.options.os == crate::options::TargetOs::Windows),
            "linux" => Ok(self.options.os == crate::options::TargetOs::Linux),
            "macos" => Ok(self.options.os == crate::options::TargetOs::Macos),
            "freebsd" => Ok(self.options.os == crate::options::TargetOs::Freebsd),
            "unix" => Ok(self.options.os.is_unix()),
            "shell" => {
                if !self.eat(b"(") {
                    return Err((self.index, "expected `(` after `shell`".into()));
                }
                self.whitespace();
                let quote = *self
                    .input
                    .get(self.index)
                    .ok_or_else(|| (self.index, "expected a quoted shell name".into()))?;
                if quote != b'\'' && quote != b'"' {
                    return Err((self.index, "expected a quoted shell name".into()));
                }
                self.index += 1;
                let shell_start = self.index;
                while self.index < self.input.len() && self.input[self.index] != quote {
                    if self.input[self.index] == b'\\' {
                        return Err((
                            self.index,
                            "escapes are not supported in shell predicates".into(),
                        ));
                    }
                    self.index += 1;
                }
                if self.index == self.input.len() {
                    return Err((shell_start, "unterminated shell name".into()));
                }
                let shell =
                    std::str::from_utf8(&self.input[shell_start..self.index]).unwrap_or_default();
                self.index += 1;
                if !self.eat(b")") {
                    return Err((self.index, "expected `)` after shell name".into()));
                }
                // Shell names select the compilation target. Other shell names
                // are valid predicates and evaluate false for this target.
                Ok(match shell {
                    "bash" => self.options.target == crate::options::Target::Bash,
                    "pwsh" | "powershell" => self.options.target == crate::options::Target::Pwsh,
                    _ => false,
                })
            }
            _ => Err((start, format!("unknown platform predicate `{name}`"))),
        }
    }

    fn identifier(&mut self) -> Option<String> {
        self.whitespace();
        let start = self.index;
        while self.index < self.input.len()
            && (self.input[self.index].is_ascii_alphanumeric() || self.input[self.index] == b'_')
        {
            self.index += 1;
        }
        (self.index > start)
            .then(|| String::from_utf8_lossy(&self.input[start..self.index]).into_owned())
    }
}

fn skip_space_and_comments(source: &str, mut index: usize) -> usize {
    let bytes = source.as_bytes();
    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index + 1 < bytes.len() && bytes[index] == b'/' && bytes[index + 1] == b'/' {
            if starts_preprocessor_directive(source, index) {
                return index;
            }
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if index + 1 < bytes.len() && bytes[index] == b'/' && bytes[index + 1] == b'*' {
            index += 2;
            while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                index += 1;
            }
            index = (index + 2).min(bytes.len());
        } else {
            return index;
        }
    }
}

fn starts_preprocessor_directive(source: &str, index: usize) -> bool {
    let bytes = source.as_bytes();
    if index + 1 >= bytes.len() || bytes[index] != b'/' || bytes[index + 1] != b'/' {
        return false;
    }
    let mut end = index + 2;
    while end < bytes.len() && bytes[end] != b'\n' {
        end += 1;
    }
    let mut content_end = end;
    if content_end > index + 2 && bytes[content_end - 1] == b'\r' {
        content_end -= 1;
    }
    let text = source[index + 2..content_end].trim();
    ["#if", "#elif", "#else", "#endif"].iter().any(|directive| {
        text.strip_prefix(directive).is_some_and(|rest| {
            rest.is_empty() || rest.chars().next().is_some_and(char::is_whitespace)
        })
    }) || text.strip_prefix("@cfg").is_some_and(|rest| {
        rest.starts_with('(') || rest.chars().next().is_some_and(char::is_whitespace)
    })
}

/// Find the end of the next TypeScript statement without mistaking braces in
/// strings, comments, or templates for statement boundaries. This is purposely
/// a lexical scan; inactive code is not parsed as TypeScript.
fn statement_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if start >= bytes.len() {
        return None;
    }
    let first_word_end = scan_word(bytes, start);
    let first_word = std::str::from_utf8(&bytes[start..first_word_end]).unwrap_or("");
    let function_declaration = first_word == "function";
    let declaration =
        function_declaration || matches!(first_word, "class" | "interface" | "enum" | "namespace");
    let blockish = matches!(first_word, "if" | "while" | "for" | "switch" | "try" | "do")
        || bytes[start] == b'{';
    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    let mut seen_outer_block = false;
    let mut index = start;
    let mut last_significant = start;
    let mut previous_block_closed = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' => {
                let quote = bytes[index];
                index = skip_quoted(bytes, index + 1, quote);
                last_significant = index.saturating_sub(1);
                previous_block_closed = false;
            }
            b'`' => {
                index = skip_quoted(bytes, index + 1, b'`');
                last_significant = index.saturating_sub(1);
                previous_block_closed = false;
            }
            b'/' if index + 1 < bytes.len() && bytes[index + 1] == b'/' => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if index + 1 < bytes.len() && bytes[index + 1] == b'*' => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            b'(' => {
                parens += 1;
                index += 1;
                previous_block_closed = false;
            }
            b')' => {
                parens = parens.saturating_sub(1);
                index += 1;
                last_significant = index - 1;
            }
            b'[' => {
                brackets += 1;
                index += 1;
                previous_block_closed = false;
            }
            b']' => {
                brackets = brackets.saturating_sub(1);
                index += 1;
                last_significant = index - 1;
            }
            b'{' => {
                braces += 1;
                if parens == 0 && brackets == 0 {
                    seen_outer_block = true;
                }
                index += 1;
                previous_block_closed = false;
            }
            b'}' => {
                braces = braces.saturating_sub(1);
                index += 1;
                last_significant = index - 1;
                if braces == 0 && parens == 0 && brackets == 0 && seen_outer_block {
                    previous_block_closed = true;
                    if blockish && !declaration {
                        let after = skip_space_and_comments(source, index);
                        let word_end = scan_word(bytes, after);
                        let word = std::str::from_utf8(&bytes[after..word_end]).unwrap_or("");
                        if !matches!(word, "else" | "catch" | "finally") {
                            return Some(index);
                        }
                    } else if declaration {
                        // Function return type literals can precede the actual
                        // function body. Continue if another top-level `{`
                        // follows; otherwise this closing brace ended it.
                        let after = skip_space_and_comments(source, index);
                        if function_declaration && after < bytes.len() && bytes[after] == b'{' {
                            seen_outer_block = false;
                        } else {
                            return Some(index);
                        }
                    }
                }
            }
            b';' if parens == 0 && brackets == 0 && braces == 0 => return Some(index + 1),
            b'\n' if parens == 0 && brackets == 0 && braces == 0 => {
                if previous_block_closed {
                    let after = skip_space_and_comments(source, index + 1);
                    let word_end = scan_word(bytes, after);
                    let word = std::str::from_utf8(&bytes[after..word_end]).unwrap_or("");
                    if matches!(word, "else" | "catch" | "finally")
                        || (matches!(first_word, "import" | "export") && word == "from")
                    {
                        index += 1;
                        continue;
                    }
                    return Some(index);
                }
                let previous = bytes.get(last_significant).copied().unwrap_or_default();
                let after = skip_space_and_comments(source, index + 1);
                if !continues_line(previous, bytes.get(after).copied()) {
                    return Some(index);
                }
                index += 1;
            }
            byte if byte.is_ascii_whitespace() => index += 1,
            _ => {
                last_significant = index;
                previous_block_closed = false;
                index += 1;
            }
        }
    }
    Some(bytes.len())
}

fn scan_word(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len()
        && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_' || bytes[index] == b'$')
    {
        index += 1;
    }
    index
}

fn skip_quoted(bytes: &[u8], mut index: usize, quote: u8) -> usize {
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
        } else if bytes[index] == quote {
            return index + 1;
        } else {
            index += 1;
        }
    }
    index
}

fn continues_line(previous: u8, next: Option<u8>) -> bool {
    matches!(
        previous,
        b'=' | b'+'
            | b'-'
            | b'*'
            | b'/'
            | b'&'
            | b'|'
            | b'?'
            | b':'
            | b','
            | b'.'
            | b'<'
            | b'>'
            | b'('
            | b'['
            | b'\\'
    ) || next.is_some_and(|byte| {
        matches!(
            byte,
            b'.' | b'(' | b'[' | b'+' | b'-' | b'*' | b'/' | b'&' | b'|' | b'?' | b':' | b','
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::TargetOs;
    use std::path::Path;

    fn options(os: TargetOs) -> CompileOptions {
        CompileOptions {
            os,
            ..CompileOptions::default()
        }
    }

    #[test]
    fn preprocesses_platform_branches_and_preserves_offsets() {
        let source = "// #if windows\nlet invalid: ???;\n// #elif linux && !macos\nconst ok = 1;\n// #else\nother ???\n// #endif\n";
        let result = preprocess(Path::new("main.tsh"), source, &options(TargetOs::Linux)).unwrap();
        assert_eq!(source.len(), result.len());
        assert!(result.contains("const ok = 1;"));
        assert!(!result.contains("invalid"));
        assert!(!result.contains("other"));
    }

    #[test]
    fn ignores_directive_text_in_templates_and_strings() {
        let source =
            "const x = `raw\n// #if windows\ninvalid ???\n`;\nconst y = '// #if windows';\n";
        let result = preprocess(Path::new("main.tsh"), source, &options(TargetOs::Linux)).unwrap();
        assert_eq!(result, source);
    }

    #[test]
    fn cfg_keeps_matching_statement_and_excludes_nonmatching_statement() {
        let source = "// @cfg(windows)\nfunction nope() {\n  return 1;\n}\nconst yes = 2;\n";
        let windows =
            preprocess(Path::new("main.tsh"), source, &options(TargetOs::Windows)).unwrap();
        assert!(windows.contains("function nope"));
        let linux = preprocess(Path::new("main.tsh"), source, &options(TargetOs::Linux)).unwrap();
        assert!(!linux.contains("function nope"));
        assert!(linux.contains("const yes = 2;"));
        assert_eq!(source.len(), linux.len());
    }

    #[test]
    fn malformed_and_unmatched_directives_are_diagnostics() {
        assert!(
            preprocess(
                Path::new("main.tsh"),
                "// #if nope\nx;\n// #endif\n",
                &CompileOptions::default()
            )
            .is_err()
        );
        assert!(
            preprocess(
                Path::new("main.tsh"),
                "// #endif\n",
                &CompileOptions::default()
            )
            .is_err()
        );
    }

    #[test]
    fn cfg_statement_scan_handles_declarations_control_blocks_and_following_statements() {
        let source = concat!(
            "// @cfg(windows)\nclass HiddenClass { method() { return { nested: true }; } }\n",
            "const afterClass = 1;\n",
            "// @cfg(windows)\nfunction hiddenFunction(): { value: number } { return { value: 1 }; }\n",
            "const afterFunction = 2;\n",
            "// @cfg(windows)\nif (true) { let x = 1; } else { let x = 2; }\n",
            "const afterIf = 3;\n",
            "// @cfg(windows)\nfor (let i = 0; i < 1; i++) { work(); }\n",
            "const afterFor = 4;\n",
            "// @cfg(windows)\nimport {\n  missing,\n}\nfrom './not-a-module';\n",
            "const afterImport = 5;\n",
        );
        let result = preprocess(Path::new("main.tsh"), source, &options(TargetOs::Linux)).unwrap();
        for hidden in [
            "HiddenClass",
            "hiddenFunction",
            "work();",
            "missing",
            "from './not-a-module'",
        ] {
            assert!(
                !result.contains(hidden),
                "statement `{hidden}` was not removed"
            );
        }
        for visible in [
            "afterClass",
            "afterFunction",
            "afterIf",
            "afterFor",
            "afterImport",
        ] {
            assert!(
                result.contains(visible),
                "following statement `{visible}` was also removed"
            );
        }
        assert_eq!(source.len(), result.len());
    }

    #[test]
    fn nested_conditionals_and_unicode_keep_original_byte_offsets() {
        let source = concat!(
            "const greeting = 'नमस्ते';\n",
            "// #if windows\n",
            "// #if linux\nlet invalid: ???;\n// #endif\n",
            "// #else\nconst linuxOnly = '你好';\n// #endif\n",
        );
        let result =
            preprocess(Path::new("unicode.tsh"), source, &options(TargetOs::Linux)).unwrap();
        assert_eq!(source.len(), result.len());
        assert!(result.contains("const greeting = 'नमस्ते';"));
        assert!(result.contains("const linuxOnly = '你好';"));
        assert!(!result.contains("invalid"));
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }

    #[test]
    fn cfg_without_a_following_statement_does_not_consume_later_source() {
        let source = "// @cfg(windows)\n// #if linux\nconst stillThere = 1;\n// #endif\n";
        let result = preprocess(Path::new("main.tsh"), source, &options(TargetOs::Linux)).unwrap();
        assert!(result.contains("const stillThere = 1;"));
    }
}
