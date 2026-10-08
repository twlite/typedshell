use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
#[error("{message}")]
pub struct CompileError {
    pub message: String,
    #[source_code]
    pub source_code: NamedSource<String>,
    #[label("{message}")]
    pub span: SourceSpan,
}
impl CompileError {
    pub fn new(
        name: impl Into<String>,
        source: &str,
        start: usize,
        len: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            message: message.into(),
            source_code: NamedSource::new(name.into(), source.to_owned()),
            span: (
                start.min(source.len()),
                len.min(source.len().saturating_sub(start)),
            )
                .into(),
        }
    }
    pub fn plain(message: impl Into<String>) -> Self {
        Self::new("tsh", "", 0, 0, message)
    }
}
