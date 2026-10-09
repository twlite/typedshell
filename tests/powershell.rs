use std::{
    process::{Command, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};
use tsh::{Comments, CompileOptions, Target, TargetOs, compile_source};

static HAS_PWSH: OnceLock<bool> = OnceLock::new();

fn has_pwsh() -> bool {
    *HAS_PWSH.get_or_init(|| {
        Command::new("pwsh")
            .args(["-NoProfile", "-Command", "$PSVersionTable.PSVersion.Major"])
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

fn options() -> CompileOptions {
    CompileOptions {
        target: Target::Pwsh,
        ..CompileOptions::default()
    }
}

fn compile(source: &str) -> String {
    compile_source("test.tsh", source, &options()).unwrap_or_else(|e| panic!("{e:?}"))
}

fn compile_with(source: &str, options: &CompileOptions) -> String {
    compile_source("test.tsh", source, options).unwrap_or_else(|e| panic!("{e:?}"))
}

fn execute(source: &str) -> (i32, String, String) {
    execute_script(&compile(source))
}

fn execute_script(script: &str) -> (i32, String, String) {
    if !has_pwsh() {
        eprintln!("skipping PowerShell execution: `pwsh` is not installed");
        return (0, String::new(), String::new());
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("standalone.ps1");
    std::fs::write(&path, script).unwrap();
    let mut child = Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(&path)
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("pwsh is required for PowerShell integration tests");
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("generated PowerShell exceeded 30 seconds: {script}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn requires_pwsh() {
    if !has_pwsh() {
        eprintln!("skipping PowerShell execution test: `pwsh` is not installed");
        panic!("skipped: pwsh is not installed");
    }
}

fn output(source: &str) -> String {
    requires_pwsh();
    let (code, out, err) = execute(source);
    assert_eq!(code, 0, "{err}");
    // PowerShell uses CRLF on Windows but LF elsewhere; normalize for asserts.
    out.replace("\r\n", "\n")
}

fn rejects(source: &str) {
    assert!(
        compile_source("invalid.tsh", source, &options()).is_err(),
        "unexpectedly accepted: {source}"
    );
}

#[test]
fn pwsh_variables_and_reassignment() {
    assert_eq!(
        output("const name = 'Twilight'; let count = 0; count++; echo(name); echo(count);"),
        "Twilight\n1\n"
    );
}

#[test]
fn pwsh_strings_and_interpolation() {
    assert_eq!(
        output("const who: string = 'world'; echo(`Hello ${who}!`);"),
        "Hello world!\n"
    );
    // Static strings must survive shell metacharacters without execution.
    let source = r#"const value = "a' b \" c $HOME $(touch INJECTED) `uname` \\ end"; echo(value); run("test", ["!", "-e", "INJECTED"]);"#;
    assert_eq!(
        output(source),
        "a' b \" c $HOME $(touch INJECTED) `uname` \\ end\n"
    );
}

#[test]
fn pwsh_arithmetic_and_comparisons() {
    assert_eq!(
        output(
            "let n: number = 2; n = n * 3 + 1; echo(n); echo(10 % 3); echo(7 / 2); echo(-7 / 2);"
        ),
        "7\n1\n3\n-3\n"
    );
    assert_eq!(output("echo(9007199254740993);"), "9007199254740993\n");
    assert_eq!(
        output(
            "if (1 < 2) { echo('lt'); } if (2 <= 2) { echo('le'); } if (3 > 2) { echo('gt'); } if (3 >= 4) { echo('no'); } else { echo('ge-no'); }"
        ),
        "lt\nle\ngt\nge-no\n"
    );
}

#[test]
fn pwsh_string_comparisons_are_case_sensitive() {
    assert_eq!(
        output("if ('abc' === 'ABC') { echo('same'); } else { echo('different'); }"),
        "different\n"
    );
    assert_eq!(
        output("if ('abc' !== 'ABC') { echo('different'); } else { echo('same'); }"),
        "different\n"
    );
    assert_eq!(
        output("const yes = true; echo(yes); echo(`value=${!yes}`);"),
        "true\nvalue=false\n"
    );
}

#[test]
fn pwsh_functions_return_values_without_pipeline_leaks() {
    assert_eq!(
        output("function add(a: number, b: number): number { return a + b; } echo(add(10, 20));"),
        "30\n"
    );
    // Intermediate statements must not leak into the return value.
    assert_eq!(
        output(
            "function calculate(): number { const x = 10; const y = 20; return x + y; } echo(calculate());"
        ),
        "30\n"
    );
    // A discarded non-void call inside a function stays discarded.
    assert_eq!(
        output(
            "function add(a: number, b: number): number { return a + b; } function caller(): number { add(1, 2); return 5; } echo(caller());"
        ),
        "5\n"
    );
}

#[test]
fn pwsh_void_functions_do_not_write_to_output() {
    assert_eq!(
        output("function quiet(): void { const ignored = 1; } quiet(); echo('after');"),
        "after\n"
    );
}

#[test]
fn pwsh_global_mutation_survives_calls() {
    assert_eq!(
        output(
            "let total = 0; function bump(n: number): number { total = total + n; return total; } const a = bump(2); const b = bump(3); echo(`${a}:${b}:${total}`);"
        ),
        "2:5:5\n"
    );
    assert_eq!(
        output(
            "function factorial(n: number): number { if (n <= 1) { return 1; } return n * factorial(n - 1); } echo(factorial(6));"
        ),
        "720\n"
    );
}

#[test]
fn pwsh_classes_constructors_and_instances() {
    assert_eq!(
        output(
            r#"class User { constructor(public name: string) {} greet(): void { echo(`Hello, ${this.name}!`); } rename(name: string): void { this.name = name; } } const first = new User("Twilight"); const second = new User("Luna"); first.greet(); first.rename("Arch"); first.greet(); second.greet();"#
        ),
        "Hello, Twilight!\nHello, Arch!\nHello, Luna!\n"
    );
}

#[test]
fn pwsh_method_calls_and_static_methods() {
    assert_eq!(
        output(
            "class Counter { value: number = 0; constructor() {} next(): number { this.value = this.value + 1; return this.value; } static label(): string { return 'counter'; } } const c = new Counter(); const a = c.next(); const b = c.next(); echo(`${Counter.label()}:${a}:${b}:${c.value}`);"
        ),
        "counter:1:2:2\n"
    );
    assert_eq!(
        output(
            "class C { constructor(public value: number) {} get(): number { return this.value; } plus(other: C): number { return other.get() + this.get(); } } const a = new C(2); const b = new C(5); echo(a.plus(b));"
        ),
        "7\n"
    );
}

#[test]
fn pwsh_control_flow() {
    assert_eq!(
        output(
            "let n = 0; while (n < 2) { echo(n); n = n + 1; } for (let i = 0; i < 3; i++) { if (i === 1) { echo('one'); } else { echo(i); } }"
        ),
        "0\n1\n0\none\n2\n"
    );
    assert_eq!(
        output(
            "for (let i = 0; i < 4; i++) { if (i === 1) { continue; } if (i === 3) { break; } echo(i); }"
        ),
        "0\n2\n"
    );
    assert_eq!(
        output(
            "let n = 0; function tick(): boolean { n = n + 1; return true; } const a = false && tick(); const b = true || tick(); echo(n); const c = true && tick(); echo(n);"
        ),
        "0\n1\n"
    );
}

#[test]
fn pwsh_filesystem_intrinsics() {
    // Note: `/dev/null` is intentionally avoided; `Copy-Item` cannot copy
    // device files, so the source file is created with a nested `pwsh`.
    assert_eq!(
        output(
            r#"mkdir("a directory", { recursive: true }); echo("Done!"); run("pwsh", ["-NoProfile", "-Command", "'x' | Set-Content -LiteralPath 'a directory/file' -NoNewline"]); cp("a directory/file", "a directory/moved"); mv("a directory/moved", "a directory/file2"); cd("a directory"); run("pwsh", ["-NoProfile", "-Command", "if (-not (Test-Path -LiteralPath 'file2')) { exit 1 }"]); cd(".."); rm("a directory", { recursive: true });"#
        ),
        "Done!\n"
    );
}

#[test]
fn pwsh_external_commands_and_escaping() {
    assert_eq!(
        output(r#"run("printf", ["<%s>\n", "a b", "$(exit 99)", "*", ""]);"#),
        "<a b>\n<$(exit 99)>\n<*>\n<>\n"
    );
    // Special characters in arguments must not break out of safe invocation.
    assert_eq!(
        output(r#"run("printf", ["<%s>\n", "it's $HOME `x` \"q\"; rm -rf /"]); "#),
        "<it's $HOME `x` \"q\"; rm -rf />\n"
    );
}

#[test]
fn pwsh_exit_code_propagation() {
    requires_pwsh();
    let (code, out, _) = execute("run('bash', ['-c', 'exit 17']); echo('unreachable');");
    assert_eq!(code, 17);
    assert!(out.replace("\r\n", "\n").is_empty());
    assert_eq!(execute("exit(23);").0, 23);
}

#[test]
fn pwsh_env_without_execution() {
    requires_pwsh();
    let script = compile("echo(env('TSH_TEST_ENV'));");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.ps1");
    std::fs::write(&path, script).unwrap();
    let out = Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(path)
        .env("TSH_TEST_ENV", "a b $HOME")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n"),
        "a b $HOME\n"
    );
}

#[test]
fn pwsh_typedshell_host_info_and_environment_api() {
    let out = output(
        "echo(TypedShell.platform()); echo(TypedShell.arch()); echo(TypedShell.homedir() != ''); TypedShell.setEnv('TSH_TYPEDSHELL_API', 'a b $HOME'); echo(env('TSH_TYPEDSHELL_API'));",
    );
    let mut lines = out.lines();
    let expected_platform = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        "windows" => "windows",
        other => other,
    };
    let expected_arch = match std::env::consts::ARCH {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        "x86" | "i386" | "i586" | "i686" => "x86",
        "arm" | "armv7" | "armv7l" => "arm",
        other => other,
    };
    assert_eq!(lines.next(), Some(expected_platform));
    assert_eq!(lines.next(), Some(expected_arch));
    assert_eq!(lines.next(), Some("true"));
    assert_eq!(lines.next(), Some("a b $HOME"));
    assert_eq!(lines.next(), None);
}

#[test]
fn pwsh_add_path_is_idempotent_and_persists_to_user_profile() {
    requires_pwsh();
    if cfg!(windows) {
        // The Windows implementation updates the current user's PATH registry
        // value, so keep this filesystem integration test on Unix hosts.
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let path_entry = format!("{}/tools with ' quote", home.path().display());
    let source = format!(
        "TypedShell.addPath(\"{path_entry}\"); TypedShell.addPath(\"{path_entry}\"); echo(env('PATH'));"
    );
    let script_dir = tempfile::tempdir().unwrap();
    let script_path = script_dir.path().join("add-path.ps1");
    std::fs::write(&script_path, compile(&source)).unwrap();
    let out = Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(&script_path)
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let path_output = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        path_output.matches(&path_entry).count(),
        1,
        "PATH output was: {path_output:?}"
    );

    let profile = home.path().join(".config/powershell/profile.ps1");
    let profile_contents = std::fs::read_to_string(&profile).unwrap();
    assert_eq!(profile_contents.matches("$candidate = ").count(), 1);
    let profile_result = Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(&profile)
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        profile_result.status.success(),
        "{}",
        String::from_utf8_lossy(&profile_result.stderr)
    );

    let verify_script = script_dir.path().join("verify-profile.ps1");
    let profile_literal = profile.to_string_lossy().replace("'", "''");
    std::fs::write(
        &verify_script,
        format!(". '{profile_literal}'; . '{profile_literal}'; [Console]::WriteLine($env:PATH)"),
    )
    .unwrap();
    let profile_output = Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(verify_script)
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        profile_output.status.success(),
        "{}",
        String::from_utf8_lossy(&profile_output.stderr)
    );
    assert_eq!(
        String::from_utf8(profile_output.stdout)
            .unwrap()
            .matches(&path_entry)
            .count(),
        1
    );
}

#[test]
fn pwsh_os_specific_imports_follow_target_os() {
    let unix_options = CompileOptions {
        os: TargetOs::Macos,
        ..options()
    };
    // `tsh:unix` is available on macOS targets, `tsh:windows` is rejected.
    assert!(
        compile_source(
            "cfg.tsh",
            "import { chmod } from 'tsh:unix';",
            &unix_options
        )
        .is_ok()
    );
    assert!(compile_source("cfg.tsh", "import {} from 'tsh:windows';", &unix_options).is_err());
    // `shell("pwsh")` selects the PowerShell backend only.
    let selected = compile_with(
        "// #if shell(\"pwsh\")\necho('pwsh');\n// #else\necho('other');\n// #endif",
        &options(),
    );
    assert!(selected.contains("pwsh"));
    assert!(!selected.contains("'other'"));
    let bash_options = CompileOptions {
        target: Target::Bash,
        ..CompileOptions::default()
    };
    let other = compile_source(
        "cfg.tsh",
        "// #if shell(\"pwsh\")\necho('pwsh');\n// #else\necho('other');\n// #endif",
        &bash_options,
    )
    .unwrap();
    assert!(!other.contains("pwsh"));
}

#[test]
fn pwsh_conditional_compilation() {
    let opts = CompileOptions {
        os: TargetOs::Macos,
        ..options()
    };
    let script = compile_source(
        "cfg.tsh",
        "// #if windows\nnot even valid TypeScript !!!\n// #elif unix && shell(\"pwsh\")\necho('unix pwsh');\n// #else\necho('other');\n// #endif\n",
        &opts,
    )
    .unwrap();
    assert!(script.contains("unix pwsh"));
    assert!(!script.contains("not even"));
}

#[test]
fn pwsh_raw_insertion() {
    assert_eq!(
        output("echo('before'); raw!(`Write-Output 'raw'`); echo('after');"),
        "before\nraw\nafter\n"
    );
    // Raw PowerShell is only compiled for the PowerShell target.
    let script = compile("echo('x');\n// @cfg(shell(\"pwsh\"))\nraw!(`Write-Output 'only pwsh'`);");
    assert!(script.contains("only pwsh"));
    let bash_options = CompileOptions {
        target: Target::Bash,
        ..CompileOptions::default()
    };
    let bash_script = compile_source(
        "cfg.tsh",
        "echo('x');\n// @cfg(shell(\"pwsh\"))\nraw!(`Write-Output 'only pwsh'`);",
        &bash_options,
    )
    .unwrap();
    assert!(!bash_script.contains("only pwsh"));
}

#[test]
fn pwsh_comment_preservation() {
    let source = "// ordinary\n/** documentation */\necho('x');";
    for mode in [Comments::All, Comments::Doc, Comments::None] {
        let script = compile_source(
            "comments.tsh",
            source,
            &CompileOptions {
                comments: mode,
                ..options()
            },
        )
        .unwrap();
        assert_eq!(script.contains("ordinary"), mode == Comments::All);
        assert_eq!(script.contains("documentation"), mode != Comments::None);
    }
    // Doc comments on methods survive compilation.
    let script = compile(
        "class C { /** Generate values. */ next(): number { return 1; } constructor() {} } const c = new C(); echo(c.next());",
    );
    assert!(script.contains("Generate values."));
}

#[test]
fn pwsh_local_module_imports() {
    requires_pwsh();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("utils.tsh"),
        "export function greet(name: string): string { return `hi ${name}`; }",
    )
    .unwrap();
    let path = dir.path().join("main.tsh");
    std::fs::write(
        &path,
        "import { greet } from './utils.tsh'; echo(greet('friend')); ",
    )
    .unwrap();
    let script = tsh::compile_file(&path, &options()).unwrap();
    std::fs::remove_file(dir.path().join("utils.tsh")).unwrap();
    let generated = dir.path().join("main.ps1");
    std::fs::write(&generated, script).unwrap();
    let out = Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(&generated)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n"),
        "hi friend\n"
    );
}

#[test]
fn pwsh_identifier_case_collisions() {
    // `foo` and `Foo` are distinct in tsh but not in PowerShell.
    assert_eq!(
        output("const foo = 'lower'; const Foo = 'upper'; echo(`${foo}:${Foo}`);"),
        "lower:upper\n"
    );
}

#[test]
fn pwsh_wildcard_paths_are_literal() {
    assert_eq!(
        output(
            "mkdir('wild[card]*?', { recursive: true }); cd('wild[card]*?'); echo('ok'); cd('..'); rm('wild[card]*?', { recursive: true });",
        ),
        "ok\n"
    );
}

#[test]
fn pwsh_type_errors_are_rejected() {
    for src in [
        "const x: number = 'string';",
        "echo(missing);",
        "const x = 1.5;",
        "let x = true + 1;",
    ] {
        rejects(src);
    }
    // String chmod modes have no PowerShell equivalent.
    rejects("import { chmod } from 'tsh:unix'; chmod('f', 'u+x');");
}

#[test]
fn cross_backend_stdout_matches() {
    requires_pwsh();
    let programs = [
        "const who: string = 'world'; echo(`Hello ${who}!`);",
        "let n = 0; for (let i = 0; i < 5; i++) { n = n + i; } echo(n);",
        "function factorial(n: number): number { if (n <= 1) { return 1; } return n * factorial(n - 1); } echo(factorial(6));",
        "class Counter { value: number = 0; constructor() {} next(): number { this.value = this.value + 1; return this.value; } } const c = new Counter(); echo(`${c.next()}:${c.next()}`);",
        "const yes = true; echo(yes); echo(!yes);",
        "echo(7 / 2); echo(10 % 3);",
        "if ('a' === 'b') { echo('x'); } else { echo('y'); }",
    ];
    for source in programs {
        let bash_script = compile_source("cross.tsh", source, &CompileOptions::default()).unwrap();
        let pwsh_script = compile(source);
        let dir = tempfile::tempdir().unwrap();
        let bash_path = dir.path().join("cross.sh");
        let pwsh_path = dir.path().join("cross.ps1");
        std::fs::write(&bash_path, bash_script).unwrap();
        std::fs::write(&pwsh_path, pwsh_script).unwrap();
        let bash_out = Command::new("bash").arg(&bash_path).output().unwrap();
        let pwsh_out = Command::new("pwsh")
            .args(["-NoProfile", "-File"])
            .arg(&pwsh_path)
            .output()
            .unwrap();
        assert_eq!(bash_out.status.code(), pwsh_out.status.code(), "{source}");
        assert_eq!(
            String::from_utf8(bash_out.stdout).unwrap(),
            String::from_utf8(pwsh_out.stdout)
                .unwrap()
                .replace("\r\n", "\n"),
            "{source}"
        );
    }
}

#[test]
fn pwsh_standalone_ps1_without_tsh() {
    requires_pwsh();
    // The generated script must run without any tsh runtime or helpers.
    let script = compile("echo('standalone');");
    assert!(!script.contains("tsh-runtime"));
    let (code, out, _) = execute_script(&script);
    assert_eq!(code, 0);
    assert_eq!(out.replace("\r\n", "\n"), "standalone\n");
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tsh"))
}

#[test]
fn pwsh_cli_compiles_checks_and_executes() {
    requires_pwsh();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("hello.tsh");
    std::fs::write(&source, "echo('hello pwsh');").unwrap();
    let checked = cli()
        .args(["check", "--target", "pwsh"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(checked.status.success(), "{checked:?}");
    let compiled = cli()
        .arg("compile")
        .arg(&source)
        .args(["--target", "pwsh", "--os", "macos", "--comments", "none"])
        .output()
        .unwrap();
    assert!(compiled.status.success(), "{compiled:?}");
    let generated = source.with_extension("ps1");
    assert!(generated.exists());
    let out = Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(&generated)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n"),
        "hello pwsh\n"
    );
    // Direct execution leaves no permanent artifact behind.
    let out = cli()
        .arg(&source)
        .args(["--target", "pwsh"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n"),
        "hello pwsh\n"
    );
}

#[test]
fn pwsh_cli_forwards_arguments_stdin_and_exit_status() {
    use std::io::Write;
    requires_pwsh();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("args.tsh");
    std::fs::write(
        &source,
        "raw!(`foreach ($a in $args) { [Console]::WriteLine(\"<\" + $a + \">\") }\n$line = [Console]::In.ReadLine()\n[Console]::WriteLine($line)\n`); exit(19);",
    )
    .unwrap();
    let mut child = cli()
        .arg(&source)
        .args(["--target", "pwsh", "--", "--verbose", "a b", "$(uname)", ""])
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
    assert_eq!(out.status.code(), Some(19), "{out:?}");
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n"),
        "<--verbose>\n<a b>\n<$(uname)>\n<>\ninput preserved\n"
    );
    assert!(!source.with_extension("ps1").exists());
}

#[test]
#[cfg(unix)]
fn pwsh_chmod_sets_unix_modes() {
    use std::os::unix::fs::PermissionsExt;
    requires_pwsh();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f"), "x").unwrap();
    let path = dir.path().join("chmod.tsh");
    std::fs::write(
        &path,
        "import { chmod } from 'tsh:unix'; chmod('f', 0o755); echo('mode-ok');",
    )
    .unwrap();
    let script = tsh::compile_file(
        &path,
        &CompileOptions {
            os: TargetOs::Linux,
            ..options()
        },
    )
    .unwrap();
    let generated = dir.path().join("chmod.ps1");
    std::fs::write(&generated, script).unwrap();
    let out = Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(&generated)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    assert_eq!(
        std::fs::metadata(dir.path().join("f"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}
