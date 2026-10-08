pub mod cfg;
pub mod diagnostics;
pub mod resolver;
pub mod semantic;

use crate::{
    backends::{Backend, bash::Bash, powershell::PowerShell},
    options::{CompileOptions, Target},
};
use diagnostics::CompileError;
use std::path::Path;

pub fn check_file(
    path: impl AsRef<Path>,
    options: &CompileOptions,
) -> Result<crate::ir::Program, CompileError> {
    let sources = resolver::load(path.as_ref(), options)?;
    semantic::analyze(&sources, options)
}
pub fn compile_file(
    path: impl AsRef<Path>,
    options: &CompileOptions,
) -> Result<String, CompileError> {
    backend(options.target).emit(&check_file(path, options)?)
}
pub fn compile_source(
    name: impl AsRef<Path>,
    source: &str,
    options: &CompileOptions,
) -> Result<String, CompileError> {
    let sources = resolver::load_source(name.as_ref(), source, options)?;
    backend(options.target).emit(&semantic::analyze(&sources, options)?)
}

fn backend(target: Target) -> impl Backend {
    match target {
        Target::Bash => BackendRef::Bash(Bash),
        Target::Pwsh => BackendRef::PowerShell(PowerShell),
    }
}

enum BackendRef {
    Bash(Bash),
    PowerShell(PowerShell),
}

impl Backend for BackendRef {
    fn emit(&self, program: &crate::ir::Program) -> Result<String, CompileError> {
        match self {
            BackendRef::Bash(backend) => backend.emit(program),
            BackendRef::PowerShell(backend) => backend.emit(program),
        }
    }
}
