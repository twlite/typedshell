//! Execute a temporary generated script, retaining stdin for the program.
use std::{
    ffi::OsString,
    io::Write,
    process::{Command, ExitStatus, Stdio},
};

/// A private temporary file avoids argv size limits and leaves stdin inherited.
/// The file is deleted on return; no permanent build artifact is produced.
pub fn run(script: &str, args: &[OsString]) -> std::io::Result<ExitStatus> {
    let mut file = tempfile::Builder::new()
        .prefix("tsh-")
        .suffix(".sh")
        .tempfile()?;
    file.write_all(script.as_bytes())?;
    file.flush()?;
    Command::new("bash")
        .arg(file.path())
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
}

/// Execute a generated PowerShell script with `pwsh`, inheriting stdio,
/// environment, and the working directory. Arguments are passed as separate
/// process arguments, never concatenated into a shell string.
pub fn run_pwsh(script: &str, args: &[OsString]) -> std::io::Result<ExitStatus> {
    let mut file = tempfile::Builder::new()
        .prefix("tsh-")
        .suffix(".ps1")
        .tempfile()?;
    file.write_all(script.as_bytes())?;
    file.flush()?;
    Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(file.path())
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
}
