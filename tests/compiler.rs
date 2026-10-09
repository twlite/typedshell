use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tsh::{Comments, CompileOptions, TargetOs, compile_source};

fn compile(source: &str) -> String {
    compile_source("test.tsh", source, &CompileOptions::default())
        .unwrap_or_else(|e| panic!("{e:?}"))
}
fn execute(source: &str) -> (i32, String, String) {
    execute_with_env(source, &[])
}

fn execute_with_env(source: &str, environment: &[(&str, &str)]) -> (i32, String, String) {
    let script = compile(source);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("standalone.sh");
    std::fs::write(&path, script).unwrap();
    let mut child = Command::new("bash")
        .arg(&path)
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(environment.iter().copied())
        .spawn()
        .expect("Bash required for integration tests");
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("generated Bash exceeded 10 seconds: {source}");
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
fn output(source: &str) -> String {
    let (code, out, err) = execute(source);
    assert_eq!(code, 0, "{err}");
    out
}

fn bash_path(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    if let Some((drive, rest)) = path.split_once(":/")
        && drive.len() == 1
    {
        return format!(
            "/{}/{}",
            drive.to_ascii_lowercase(),
            rest.trim_start_matches('/')
        );
    }
    path
}
fn rejects(source: &str) {
    assert!(
        compile_source("invalid.tsh", source, &CompileOptions::default()).is_err(),
        "unexpectedly accepted: {source}"
    );
}

#[test]
fn basic_intrinsics_and_files() {
    assert_eq!(
        output(
            r#"mkdir("a directory", { recursive: true }); echo("Done!"); cp("/dev/null", "a directory/file"); mv("a directory/file", "a directory/moved"); cd("a directory"); run("test", ["-f", "moved"]); cd(".."); rm("a directory", { recursive: true });"#
        ),
        "Done!\n"
    );
}
#[test]
fn leading_dash_paths_are_not_options() {
    assert_eq!(
        output(
            r#"mkdir("-p"); run("touch", ["--", "-f"]); cp("-f", "-g"); mv("-g", "-h"); cd("-p"); echo("ok"); cd(".."); rm("-f"); rm("-h"); rm("-p", { recursive: true });"#
        ),
        "ok\n"
    );
}
#[test]
fn variables_templates_and_numbers() {
    assert_eq!(
        output(
            "let n: number = 2; n = n * 3 + 1; const who: string = 'world'; echo(`Hello ${who} ${n}`);"
        ),
        "Hello world 7\n"
    );
}
#[test]
fn booleans_print_as_language_values() {
    assert_eq!(
        output("const yes = true; echo(yes); echo(`value=${!yes}`);"),
        "true\nvalue=false\n"
    );
}
#[test]
fn escaping_is_not_shell_execution() {
    let source = r#"const value = "a' b \" c $HOME $(touch INJECTED) `uname` \\ end"; echo(value); run("test", ["!", "-e", "INJECTED"]);"#;
    assert_eq!(
        output(source),
        "a' b \" c $HOME $(touch INJECTED) `uname` \\ end\n"
    );
}
#[test]
fn command_arguments_are_boundaries() {
    assert_eq!(
        output(r#"run("printf", ["<%s>\n", "a b", "$(exit 99)", "*", ""]);"#),
        "<a b>\n<$(exit 99)>\n<*>\n<>\n"
    );
}
#[test]
fn functions_return_and_mutate_without_subshell() {
    assert_eq!(
        output(
            "let total = 0; function bump(n: number): number { total = total + n; return total; } const a = bump(2); const b = bump(3); echo(`${a}:${b}:${total}`);"
        ),
        "2:5:5\n"
    );
}
#[test]
fn recursive_function_results() {
    assert_eq!(
        output(
            "function factorial(n: number): number { if (n <= 1) { return 1; } return n * factorial(n - 1); } echo(factorial(6));"
        ),
        "720\n"
    );
}
#[test]
fn string_return_preserves_trailing_newlines() {
    assert_eq!(
        output(r#"function text(): string { return "hello\n\n"; } echo(text());"#),
        "hello\n\n\n"
    );
}
#[test]
fn control_flow() {
    assert_eq!(
        output(
            "let n = 0; while (n < 2) { echo(n); n = n + 1; } for (let i = 0; i < 3; i++) { if (i === 1) { echo('one'); } else { echo(i); } }"
        ),
        "0\n1\n0\none\n2\n"
    );
}
#[test]
fn empty_and_comments_only_blocks_are_valid_shell() {
    assert_eq!(
        output("if (true) {} if (false) { echo('bad'); } else { /* empty */ } echo('ok');"),
        "ok\n"
    );
}
#[test]
fn for_continue_runs_update() {
    assert_eq!(
        output(
            "for (let i = 0; i < 4; i++) { if (i === 1) { continue; } if (i === 3) { break; } echo(i); }"
        ),
        "0\n2\n"
    );
}
#[test]
fn short_circuit_preserves_effects() {
    assert_eq!(
        output(
            "let n = 0; function tick(): boolean { n = n + 1; return true; } const a = false && tick(); const b = true || tick(); echo(n); const c = true && tick(); echo(n);"
        ),
        "0\n1\n"
    );
}
#[test]
fn classes_have_independent_mutable_identity() {
    assert_eq!(
        output(
            r#"class User { constructor(public name: string) {} greet(): void { echo(`Hello, ${this.name}!`); } rename(name: string): void { this.name = name; } } const first = new User("Twilight"); const second = new User("Luna"); first.greet(); first.rename("Arch"); first.greet(); second.greet();"#
        ),
        "Hello, Twilight!\nHello, Arch!\nHello, Luna!\n"
    );
}
#[test]
fn class_return_values_keep_mutations() {
    assert_eq!(
        output(
            "class Counter { value: number = 0; constructor() {} next(): number { this.value = this.value + 1; return this.value; } static label(): string { return 'counter'; } } const c = new Counter(); const a = c.next(); const b = c.next(); echo(`${Counter.label()}:${a}:${b}:${c.value}`);"
        ),
        "counter:1:2:2\n"
    );
}
#[test]
fn nested_method_calls_keep_this() {
    assert_eq!(
        output(
            "class C { constructor(public value: number) {} get(): number { return this.value; } plus(other: C): number { return other.get() + this.get(); } } const a = new C(2); const b = new C(5); echo(a.plus(b));"
        ),
        "7\n"
    );
}
#[test]
fn class_initializers_follow_source_order() {
    assert_eq!(
        output(
            "function mark(label: string): number { echo(label); return 1; } class C { z: number = mark('first'); a: number = mark('second'); constructor() {} } const c = new C();"
        ),
        "first\nsecond\n"
    );
}
#[test]
fn unsafe_field_names_are_rejected_or_encoded() {
    let source = "class C { 'field; touch INJECTED; #': string = 'safe'; constructor() {} } const c = new C(); run('test', ['!', '-e', 'INJECTED']); echo('safe');";
    // Rejecting this member syntax is an explicit safe subset.
    if let Ok(script) = compile_source("fields.tsh", source, &CompileOptions::default()) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script.sh");
        std::fs::write(&path, script).unwrap();
        let out = Command::new("bash")
            .arg(path)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(out.stderr.is_empty());
        assert_eq!(out.stdout, b"safe\n");
    }
}
#[test]
fn raw_at_statement_location() {
    assert_eq!(
        output("echo('before'); raw!(`printf '%s\\n' 'raw'\n# shell comment\n`); echo('after');"),
        "before\nraw\nafter\n"
    );
}
#[test]
fn raw_inside_methods_and_functions() {
    assert_eq!(
        output(
            "function f(): void { raw!(`printf 'f\\n'`); } class C { constructor() {} m(): void { raw!(`printf 'm\\n'`); } } f(); const c = new C(); c.m();"
        ),
        "f\nm\n"
    );
}
#[test]
fn raw_rejects_dynamic_and_plain_calls() {
    rejects("const command = 'echo unsafe'; raw!(command);");
    rejects("const x = 'hello'; raw!(`echo ${x}`);");
    rejects("raw('echo wrong');");
}
#[test]
fn raw_is_opaque_and_never_runs_at_compile_time() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("compile-marker");
    let source = format!(
        "raw!(`touch '{}'\nnot legal Bash [[[\n`);",
        marker.display()
    );
    let script = compile_source("raw.tsh", &source, &CompileOptions::default()).unwrap();
    assert!(script.contains("not legal Bash [[["));
    assert!(!marker.exists());
}
#[test]
fn cfg_blocks_skip_invalid_syntax() {
    let source = "// #if windows\nnot even valid TypeScript !!!\nimport {} from 'tsh:windows';\n// #elif linux || macos\necho('unix');\n// #else\necho('other');\n// #endif\n";
    let opts = CompileOptions {
        os: TargetOs::Macos,
        ..Default::default()
    };
    let script = compile_source("cfg.tsh", source, &opts).unwrap();
    assert!(script.contains("unix"));
    assert!(!script.contains("not even"));
}
#[test]
fn cfg_statements_select_target_not_host() {
    let source = "// @cfg(macos)\nconst platform = 'macOS';\n// @cfg(linux)\nconst platform = 'Linux';\necho(platform);";
    for (os, expected) in [(TargetOs::Linux, "Linux"), (TargetOs::Macos, "macOS")] {
        let opts = CompileOptions {
            os,
            ..Default::default()
        };
        let script = compile_source("cfg.tsh", source, &opts).unwrap();
        assert!(script.contains(expected));
    }
}
#[test]
fn incompatible_empty_import_rejected_and_excluded_import_skipped() {
    let opts = CompileOptions {
        os: TargetOs::Macos,
        ..Default::default()
    };
    assert!(compile_source("cfg.tsh", "import {} from 'tsh:windows';", &opts).is_err());
    assert!(
        compile_source(
            "cfg.tsh",
            "// @cfg(windows)\nimport {} from 'tsh:windows';\necho('ok');",
            &opts
        )
        .is_ok()
    );
    assert!(
        compile_source(
            "cfg.tsh",
            "// @cfg(windows)\nimport { missing } from './does-not-exist.tsh';",
            &opts
        )
        .is_ok()
    );
}
#[test]
fn cfg_boolean_predicates() {
    let opts = CompileOptions {
        os: TargetOs::Linux,
        ..Default::default()
    };
    let src = "// #if unix && !windows && (shell(\"bash\") || shell(\"zsh\"))\necho('yes');\n// #else\necho('no');\n// #endif";
    let script = compile_source("cfg.tsh", src, &opts).unwrap();
    assert!(script.contains("yes"));
    assert!(!script.contains("'no'"));
}
#[test]
fn raw_directive_lookalikes_are_opaque() {
    let src = "raw!(`\n// #if windows\n# raw body stays untouched\n`);";
    let script = compile(src);
    assert!(script.contains("// #if windows"));
}
#[test]
fn comment_modes_and_raw_comments() {
    let source =
        "// ordinary\n/** documentation */\necho('x'); raw!(`# raw forever\nprintf 'y\\n'`);";
    for mode in [Comments::All, Comments::Doc, Comments::None] {
        let script = compile_source(
            "comments.tsh",
            source,
            &CompileOptions {
                comments: mode,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(script.contains("ordinary"), mode == Comments::All);
        assert_eq!(script.contains("documentation"), mode != Comments::None);
        assert!(script.contains("# raw forever"));
    }
}
#[test]
fn type_errors_and_unsupported_syntax() {
    for src in [
        "const x: number = 'string';",
        "const x = 1; x = 2;",
        "echo(missing);",
        "async function f() {}",
        "class A extends B {}",
        "const x = Promise.resolve(1);",
        "import('fs');",
        "const x = 1.5;",
        "let x = true + 1;",
        "function f(): number { return 'bad'; }",
        "const x = () => 1;",
    ] {
        rejects(src);
    }
}
#[test]
fn spread_arguments_produce_diagnostics_without_panics() {
    for source in [
        "raw!(...['x']);",
        "echo(...['x']);",
        "run('echo', [...[]]);",
    ] {
        rejects(source);
    }
}
#[test]
fn unsupported_class_modifiers_are_not_ignored() {
    for source in [
        "@decorate class C {}",
        "class C { @decorate x: number = 1; }",
        "class C { @decorate f(): void {} }",
        "abstract class C {}",
        "declare class C {}",
        "class C<T> {}",
    ] {
        rejects(source);
    }
}
#[test]
fn integers_are_not_rounded_through_parser_floats() {
    assert_eq!(
        output("echo(9007199254740993); echo(0x20000000000001);"),
        "9007199254740993\n9007199254740993\n"
    );
}
#[test]
fn failure_propagates_actual_status() {
    let (code, out, _) = execute("run('bash', ['-c', 'exit 17']); echo('unreachable');");
    assert_eq!(code, 17);
    assert!(out.is_empty());
}
#[test]
fn explicit_exit() {
    assert_eq!(execute("exit(23);").0, 23);
}
#[test]
fn env_returns_value_without_execution() {
    let source = "echo(env('TSH_TEST_ENV'));";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.sh");
    std::fs::write(&path, compile(source)).unwrap();
    let out = Command::new("bash")
        .arg(path)
        .env("TSH_TEST_ENV", "a b $HOME")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "a b $HOME\n");
}

#[test]
fn typedshell_host_info_and_environment_api() {
    let out = output(
        "echo(TypedShell.platform()); echo(TypedShell.arch()); echo(TypedShell.homedir()); TypedShell.setEnv('TSH_TYPEDSHELL_API', 'a b $HOME'); setEnv('TSH_TYPEDSHELL_GLOBAL_API', 'global'); echo(env('TSH_TYPEDSHELL_API')); echo(env('TSH_TYPEDSHELL_GLOBAL_API'));",
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
    let expected_home = std::env::var("HOME").unwrap_or_default();
    assert_eq!(lines.next(), Some(expected_platform));
    assert_eq!(lines.next(), Some(expected_arch));
    assert_eq!(lines.next(), Some(expected_home.as_str()));
    assert_eq!(lines.next(), Some("a b $HOME"));
    assert_eq!(lines.next(), Some("global"));
    assert_eq!(lines.next(), None);
}

#[test]
fn typedshell_add_path_is_idempotent_and_quotes_profile_values() {
    let home = tempfile::tempdir().unwrap();
    let home_string = bash_path(home.path());
    let path_entry = format!("{home_string}/bin with ' quote $HOME");
    let source = format!(
        "TypedShell.addPath(\"{path_entry}\"); TypedShell.addPath(\"{path_entry}\"); echo(env('PATH'));"
    );
    let (code, output, error) =
        execute_with_env(&source, &[("HOME", &home_string), ("SHELL", "/bin/zsh")]);
    assert_eq!(code, 0, "{error}");
    assert_eq!(
        output
            .trim_end()
            .split(':')
            .filter(|entry| *entry == path_entry)
            .count(),
        1
    );
    for profile in [".zshrc", ".zprofile"] {
        let profile_path = home.path().join(profile);
        let profile_bash_path = bash_path(&profile_path);
        let contents = std::fs::read_to_string(&profile_path).unwrap();
        assert_eq!(contents.matches("case \":$PATH:\"").count(), 1);
        assert!(
            Command::new("bash")
                .args(["-n"])
                .arg(&profile_bash_path)
                .status()
                .unwrap()
                .success()
        );
        let sourced = Command::new("bash")
            .args([
                "--noprofile",
                "--norc",
                "-c",
                r#"PATH=/usr/bin; . "$1"; . "$1"; printf '%s\n' "$PATH""#,
                "bash",
            ])
            .arg(&profile_bash_path)
            .env("HOME", &home_string)
            .output()
            .unwrap();
        assert!(sourced.status.success());
        assert_eq!(
            String::from_utf8(sourced.stdout)
                .unwrap()
                .matches(&path_entry)
                .count(),
            1
        );
    }

    let fish_home = tempfile::tempdir().unwrap();
    let fish_home_string = bash_path(fish_home.path());
    let fish_path = format!("{fish_home_string}/bin with ' quote $HOME");
    let fish_source =
        format!("TypedShell.addPath(\"{fish_path}\"); TypedShell.addPath(\"{fish_path}\");");
    let (code, _, error) = execute_with_env(
        &fish_source,
        &[("HOME", &fish_home_string), ("SHELL", "/usr/bin/fish")],
    );
    assert_eq!(code, 0, "{error}");
    let fish_profile = fish_home.path().join(".config/fish/conf.d/typedshell.fish");
    let fish_contents = std::fs::read_to_string(fish_profile).unwrap();
    assert_eq!(fish_contents.matches("fish_add_path ").count(), 1);
    assert!(fish_contents.contains("\\' quote $HOME'"));
}
#[test]
fn local_modules_are_bundled() {
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
    let script = tsh::compile_file(&path, &CompileOptions::default()).unwrap();
    std::fs::remove_file(dir.path().join("utils.tsh")).unwrap();
    let generated = dir.path().join("main.sh");
    std::fs::write(&generated, script).unwrap();
    let out = Command::new("bash").arg(&generated).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "hi friend\n");
}
#[test]
fn circular_modules_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.tsh"), "import {} from './b.tsh';").unwrap();
    std::fs::write(dir.path().join("b.tsh"), "import {} from './a.tsh';").unwrap();
    let error =
        tsh::compile_file(dir.path().join("a.tsh"), &CompileOptions::default()).unwrap_err();
    assert!(error.message.contains("circular") || error.message.contains("cycle"));
}
#[test]
fn source_errors_have_source_and_span() {
    let err = compile_source(
        Path::new("broken.tsh"),
        "const x: string = 42;",
        &CompileOptions::default(),
    )
    .unwrap_err();
    assert!(!err.message.is_empty());
    assert!(err.span.offset() < 21);
}

#[test]
fn expression_evaluation_captures_values_before_later_mutations() {
    assert_eq!(
        output("let n = 1; function change(): number { n = 5; return n; } echo(n + change());"),
        "6\n"
    );
}
#[test]
fn argument_values_are_captured_in_source_order() {
    assert_eq!(
        output(
            "let n = 1; function change(): number { n = 5; return n; } function combine(a: number, b: number): number { return a * 10 + b; } echo(combine(n, change()));"
        ),
        "15\n"
    );
}
#[test]
fn lexical_shadowing() {
    assert_eq!(
        output("let x = 'outer'; if (true) { let x = 'inner'; echo(x); } echo(x);"),
        "inner\nouter\n"
    );
}
#[test]
fn shell_sensitive_environment_key() {
    assert_eq!(
        output("echo(env('bad[$(touch injected)]')); run('test', ['!', '-e', 'injected']);"),
        "\n"
    );
}
#[test]
fn rejected_values_and_illegal_control_flow() {
    for src in [
        "echo('a\\0b');",
        "let x: string; echo(x);",
        "break;",
        "continue;",
        "return 1;",
        "function f(): number { if (false) { return 1; } }",
        "class C { x: number; constructor() {} } const c = new C(); echo(c.x);",
    ] {
        rejects(src);
    }
}
#[test]
fn direct_reads_before_initialization_are_rejected() {
    rejects("const first: string = second; const second: string = 'late'; echo(first);");
    rejects("echo(value); const value = 'late';");
}
#[test]
fn indirect_reads_before_initialization_fail_at_runtime() {
    let (code, out, err) =
        execute("function read(): string { return later; } echo(read()); const later = 'late';");
    assert_ne!(code, 0);
    assert!(out.is_empty());
    assert!(!err.is_empty());
}
#[test]
fn private_module_members_do_not_leak() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("utils.tsh"),
        "const hidden = 'private'; export function greeting(): string { return hidden; }",
    )
    .unwrap();
    let path = dir.path().join("main.tsh");
    std::fs::write(
        &path,
        "import { greeting } from './utils.tsh'; echo(hidden);",
    )
    .unwrap();
    assert!(tsh::compile_file(&path, &CompileOptions::default()).is_err());
}
#[test]
fn unexported_import_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("utils.tsh"),
        "function hidden(): void { echo('private'); }",
    )
    .unwrap();
    let path = dir.path().join("main.tsh");
    std::fs::write(&path, "import { hidden } from './utils.tsh'; hidden();").unwrap();
    assert!(tsh::compile_file(&path, &CompileOptions::default()).is_err());
}
