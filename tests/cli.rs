use std::{
    io::Write,
    process::{Command, Stdio},
};
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tsh"))
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
