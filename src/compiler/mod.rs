pub mod cfg;
pub mod diagnostics;
pub mod resolver;
pub mod semantic;

use crate::{
    backends::{Backend, bash::Bash, powershell::PowerShell},
    options::{CompileOptions, Target},
};
use diagnostics::CompileError;
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// A measured stage in the file compilation pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompileStage {
    Resolve,
    Analyze,
    Emit,
}

pub fn check_file(
    path: impl AsRef<Path>,
    options: &CompileOptions,
) -> Result<crate::ir::Program, CompileError> {
    let sources = resolver::load(path.as_ref(), options)?;
    semantic::analyze(&sources, options)
}

/// Check a file and report the duration of each completed pipeline stage.
///
/// The observer is called even when a stage returns an error, so callers can
/// include failed work in their timing output.
pub fn check_file_with_observer(
    path: impl AsRef<Path>,
    options: &CompileOptions,
    observer: &mut impl FnMut(CompileStage, Duration),
) -> Result<crate::ir::Program, CompileError> {
    let started = Instant::now();
    let sources = resolver::load(path.as_ref(), options);
    observer(CompileStage::Resolve, started.elapsed());
    let sources = sources?;

    let started = Instant::now();
    let program = semantic::analyze(&sources, options);
    observer(CompileStage::Analyze, started.elapsed());
    program
}

pub fn compile_file(
    path: impl AsRef<Path>,
    options: &CompileOptions,
) -> Result<String, CompileError> {
    backend(options.target).emit(&check_file(path, options)?)
}

/// Compile a file and report the duration of each completed pipeline stage.
///
/// The observer is called even when a stage returns an error, so callers can
/// include failed work in their timing output.
pub fn compile_file_with_observer(
    path: impl AsRef<Path>,
    options: &CompileOptions,
    observer: &mut impl FnMut(CompileStage, Duration),
) -> Result<String, CompileError> {
    let program = check_file_with_observer(path, options, observer)?;

    let started = Instant::now();
    let generated = backend(options.target).emit(&program);
    observer(CompileStage::Emit, started.elapsed());
    generated
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
