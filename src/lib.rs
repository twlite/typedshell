//! Compile a statically checked TypeScript subset to standalone shell source.
//!
//! ```
//! use tsh::{compile_source, CompileOptions, TargetOs};
//!
//! let options = CompileOptions { os: TargetOs::Linux, ..Default::default() };
//! let script = compile_source("hello.tsh", "echo('Hello');", &options)?;
//! assert!(script.starts_with("#!/usr/bin/env bash"));
//! # Ok::<(), tsh::CompileError>(())
//! ```
pub mod backends;
pub mod compiler;
pub mod ir;
pub mod options;
pub mod runner;
pub use compiler::diagnostics::CompileError;
pub use compiler::{check_file, compile_file, compile_source};
pub use options::{Comments, CompileOptions, Target, TargetOs};
