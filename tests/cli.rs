use std::{
    io::Write,
    process::{Command, Stdio},
};
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tsh"))
}

#[test]
fn cli_supports_version_shortcut_and_print_mode() {
    let expected_version = format!("tsh {}\n", env!("CARGO_PKG_VERSION"));
    let long_version = cli().arg("--version").output().unwrap();
    assert!(long_version.status.success());
    assert_eq!(long_version.stdout, expected_version.as_bytes());
    let short_version = cli().arg("-v").output().unwrap();
    assert!(short_version.status.success());
    assert_eq!(short_version.stdout, expected_version.as_bytes());

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("print.tsh");
    std::fs::write(&source, "echo('compiled');").unwrap();

    let compile_print = cli()
        .arg("compile")
        .arg(&source)
        .args(["--target", "bash", "--os", "linux", "--print"])
        .output()
        .unwrap();
    assert!(compile_print.status.success(), "{compile_print:?}");
    assert!(String::from_utf8_lossy(&compile_print.stdout).contains("compiled"));
    assert!(!source.with_extension("sh").exists());

    let immediate_print = cli().arg("--print").arg(&source).output().unwrap();
    assert!(immediate_print.status.success(), "{immediate_print:?}");
    assert_eq!(immediate_print.stdout, compile_print.stdout);
    assert!(!source.with_extension("sh").exists());
}

#[test]
fn cli_verbose_reports_stage_timings_without_polluting_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("timed.tsh");
    std::fs::write(&source, "echo('timed');").unwrap();

    let checked = cli()
        .arg("check")
        .arg(&source)
        .arg("--verbose")
        .output()
        .unwrap();
    assert!(checked.status.success(), "{checked:?}");
    assert!(checked.stdout.is_empty());
    let check_stderr = String::from_utf8_lossy(&checked.stderr);
    assert!(
        check_stderr.contains("verbose: resolve imports:"),
        "{check_stderr}"
    );
    assert!(
        check_stderr.contains("verbose: semantic analysis:"),
        "{check_stderr}"
    );
    assert!(
        check_stderr.contains("verbose: check total:"),
        "{check_stderr}"
    );
    assert!(check_stderr.contains(" ms"), "{check_stderr}");

    let checked_short = cli().arg("check").arg(&source).arg("-v").output().unwrap();
    assert!(checked_short.status.success(), "{checked_short:?}");
    assert!(String::from_utf8_lossy(&checked_short.stderr).contains("verbose: check total:"));

    let compiled_print = cli()
        .arg("compile")
        .arg(&source)
        .args(["--print", "-v"])
        .output()
        .unwrap();
    assert!(compiled_print.status.success(), "{compiled_print:?}");
    assert!(String::from_utf8_lossy(&compiled_print.stdout).contains("timed"));
    let compile_stderr = String::from_utf8_lossy(&compiled_print.stderr);
    assert!(
        compile_stderr.contains("verbose: resolve imports:"),
        "{compile_stderr}"
    );
    assert!(
        compile_stderr.contains("verbose: semantic analysis:"),
        "{compile_stderr}"
    );
    assert!(
        compile_stderr.contains("verbose: backend emission:"),
        "{compile_stderr}"
    );
    assert!(
        compile_stderr.contains("verbose: compile total:"),
        "{compile_stderr}"
    );

    let compiled_file = cli()
        .arg("compile")
        .arg(&source)
        .arg("--verbose")
        .output()
        .unwrap();
    assert!(compiled_file.status.success(), "{compiled_file:?}");
    assert!(compiled_file.stdout.is_empty());
    assert!(String::from_utf8_lossy(&compiled_file.stderr).contains("verbose: write output:"));
}

#[test]
fn cli_compiles_checks_and_executes_standalone() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("hello.tsh");
    std::fs::write(&source, "echo('hello');").unwrap();
    let checked = cli().arg("check").arg(&source).output().unwrap();
    assert!(checked.status.success(), "{:?}", checked);
    let compiled = cli()
        .arg("compile")
        .arg(&source)
        .args(["--target", "bash", "--os", "linux", "--comments", "none"])
        .output()
        .unwrap();
    assert!(compiled.status.success(), "{:?}", compiled);
    let out = Command::new("bash")
        .arg(source.with_extension("sh"))
        .output()
        .unwrap();
    assert_eq!(out.stdout, b"hello\n");
    let out = cli().arg(&source).output().unwrap();
    assert_eq!(out.stdout, b"hello\n");
}
#[test]
fn cli_forwards_arguments_stdin_and_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("args.tsh");
    std::fs::write(
        &source,
        "raw!(`printf '<%s>\\n' \"$@\"\nIFS= read -r line\nprintf '%s\\n' \"$line\"\n`); exit(19);",
    )
    .unwrap();
    let mut child = cli()
        .arg(&source)
        .arg("--")
        .args(["--verbose", "a b", "$(uname)", ""])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"input preserved\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(19), "{:?}", out);
    assert_eq!(
        out.stdout,
        b"<--verbose>\n<a b>\n<$(uname)>\n<>\ninput preserved\n"
    );
    assert!(!source.with_extension("sh").exists());
}
#[test]
fn cli_handles_ts_extension_explicit_output_and_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("program.ts");
    let target = dir.path().join("output.sh");
    std::fs::write(&source, "echo('typescript syntax');").unwrap();
    let out = cli()
        .arg("compile")
        .arg(&source)
        .arg("-o")
        .arg(&target)
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    assert!(target.exists());
    std::fs::write(&source, "const value: string = 42;").unwrap();
    let out = cli().arg("check").arg(&source).output().unwrap();
    assert!(!out.status.success());
    assert!(!out.stderr.is_empty());
}
#[test]
fn cli_does_not_overwrite_source() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("program.tsh");
    std::fs::write(&source, "echo('safe');").unwrap();
    let out = cli()
        .arg("compile")
        .arg(&source)
        .arg("-o")
        .arg(&source)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "echo('safe');");
}
#[test]
fn cli_runs_generated_source_larger_than_argument_limits() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("large.tsh");
    let text = format!(
        "raw!(`\n# {}\nprintf 'large script works\\n'\n`);",
        "x".repeat(1024 * 1024)
    );
    std::fs::write(&source, text).unwrap();
    let out = cli().arg(&source).output().unwrap();
    assert!(out.status.success(), "{:?}", out.stderr);
    assert_eq!(out.stdout, b"large script works\n");
    assert!(!source.with_extension("sh").exists());
}
