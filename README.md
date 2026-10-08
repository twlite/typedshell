# TypedShell

`tsh` is an experimental language implemented in Rust that compiles a statically
checked subset of TypeScript syntax into readable, standalone shell scripts.
The Bash backend generates scripts that execute with Bash 3.2 or newer, and the
PowerShell backend generates scripts for PowerShell 7.4 or newer (PowerShell 5.1
is out of scope). Generated scripts need no JavaScript runtime or installed
`tsh` executable.

## Install

Build with Rust 1.97 or newer (required by Oxc 0.153):

```sh
cargo install --path .
# Or use cargo run -- <arguments> during development.
```

## Usage

```sh
tsh examples/hello.tsh
tsh examples/arguments.tsh -- --verbose "a path with spaces"
tsh check examples/hello.tsh
tsh compile examples/hello.tsh
tsh compile examples/hello.tsh --target bash --os linux -o hello.sh
tsh compile examples/hello.tsh --comments doc
bash hello.sh
# PowerShell backend (requires `pwsh` 7.4+ to execute)
tsh examples/powershell.tsh --target pwsh
tsh compile examples/powershell.tsh --target pwsh -o example.ps1
pwsh -NoProfile -File ./example.ps1
```

Both `.tsh` and `.ts` inputs use TypeScript parsing, independent of their extension.
The defaults are the Bash target, the compiler's host OS, all source comments,
and output beside the source with a `.sh` extension (`.ps1` for `--target pwsh`).
The available targets are `bash` and `pwsh`; the available target OS values are
`linux`, `macos`, `windows`, and `freebsd`. Choosing an OS changes conditional
compilation and module compatibility. It does not install a shell or make
platform utilities available on another system. A single `.tsh` file compiles to
both `.sh` and `.ps1`:

```sh
tsh compile examples/powershell.tsh --target bash -o example.sh
tsh compile examples/powershell.tsh --target pwsh -o example.ps1
```

Immediate execution compiles into memory and uses a private, automatically deleted
temporary script file, avoiding command-line length limits while leaving stdin
available to the program. It inherits the working directory, environment, and
standard streams, forwards each argument unchanged, and returns the script's exit
code. Bash output runs through `bash`; `--target pwsh` runs through
`pwsh -NoProfile -File` and reports a clear error when `pwsh` is not installed.
No permanent artifact is written in this mode. Generated source is never
executed through `eval` (or `Invoke-Expression`).

## Language

```ts
class User {
  constructor(public name: string) {}

  rename(name: string): void {
    this.name = name;
  }

  greet(): void {
    echo(`Hello, ${this.name}!`);
  }
}

const user = new User("Twilight");
user.greet();
user.rename("Arch");
user.greet();
```

The initial subset includes `const` and `let`, strings, integer numbers, booleans,
type annotations, templates, arithmetic and comparisons, assignments, typed
functions, `if`/`else`, `while`, and basic C-style `for` loops. Classes support
constructors, parameter properties, instance fields, mutable fields, instance
methods, static methods, and statically resolved method calls.

Class instances use integer identities and Bash indexed arrays for field storage.
Functions and methods return through an explicit result slot. Calls execute in
the current shell, so changes to objects and global variables survive returned
values. Recursive calls preserve their local values.

The PowerShell backend instead uses native PowerShell classes with typed
properties, constructors, and instance/static methods, and compiles tsh
functions to PowerShell functions. Module-scope variables are accessed through
script scope inside functions and methods so mutations survive calls, while
parameters and locals stay function-local (preserving recursion). IR names are
already collision-free; because PowerShell identifiers are case-insensitive,
uniqueness holds through the generated numeric suffixes (for example,
`__tsh_v_foo_0` versus `__tsh_v_Foo_1`). Function return values are captured
explicitly and discarded values are assigned to `$null`, so intermediate
pipeline output can never leak into a return value. Integer division uses
`[Math]::DivRem` to stay exact for 64-bit values and truncate toward zero,
matching the Bash backend. String comparisons use the case-sensitive `-c*`
operators; PowerShell's default case-insensitive operators are never used for
tsh values.

Numbers are signed shell integers, with integer division and Bash arithmetic
overflow semantics. String ordering follows the shell's locale. There is no
floating point, JavaScript coercion, or JavaScript
object model. Shell strings cannot represent NUL bytes. Shell command arguments
and interpolated values are quoted; ordinary strings are never evaluated as shell
code. `raw!()` deliberately bypasses those guarantees.

Unsupported syntax produces compiler diagnostics. Async/await, promises,
generators, inheritance, decorators, dynamic imports, arbitrary JavaScript APIs,
arrow functions, general arrays/objects, and npm/Node modules are outside this
subset. Array literals are accepted for `run` arguments and a literal options
object is accepted for `mkdir`/`rm`.

## Intrinsics

Core shell operations are globally available:

| Operation                          | Behavior                                                            |
| ---------------------------------- | ------------------------------------------------------------------- |
| `echo(value)`                      | Print one value followed by a newline                               |
| `mkdir(path, { recursive: true })` | Create a directory, optionally including parents                    |
| `rm(path, { recursive: true })`    | Remove a path, optionally recursively                               |
| `cp(source, destination)`          | Copy a file                                                         |
| `mv(source, destination)`          | Move a path                                                         |
| `cd(path)`                         | Change the script's working directory                               |
| `run(command, [args...])`          | Execute a command with separately quoted arguments                  |
| `env(name)`                        | Read an environment variable; unset variables yield an empty string |
| `exit(code)`                       | Terminate the script, defaulting to zero                            |

Native command failures terminate the script with the actual failing exit code.
The backend checks command status explicitly instead of using `set -e` as language
semantics. Raw shell controls its own failure handling and can change shell state.
The generated script depends on any native commands explicitly used by the source.

The PowerShell backend maps intrinsics to native cmdlets and .NET APIs:
`echo()` to `[Console]::WriteLine()`, `mkdir()` to `New-Item -Path`
(`-Force` for recursive), `rm()` to `Remove-Item -LiteralPath`
(`-Recurse -Force` for recursive), `cp()`/`mv()` to `Copy-Item`/`Move-Item
-LiteralPath` with `-Force`, `cd()` to `Set-Location -LiteralPath` (plus a
process-directory sync so child processes see the new directory),
`run()` to a `System.Diagnostics.Process` invocation with an exact .NET
`ArgumentList` (no wildcard expansion, boundaries preserved),
`env()` to `[System.Environment]::GetEnvironmentVariable`, and `exit()` to
`exit`. Every cmdlet failure exits 1; `run()` propagates the real exit code.
Literal paths are used throughout so wildcard characters are never expanded.

`import { chmod } from "tsh:unix"` provides `chmod(path, 0o755)` for Unix OS targets.
Built-in module compatibility is checked even for empty imports. Module names do
not imply a platform API is implemented; unavailable exports are rejected. On the
PowerShell target, numeric modes use `[System.IO.File]::SetUnixFileMode`; string
modes such as `u+x` are rejected with a diagnostic (use a numeric mode instead).

Known Bash/PowerShell differences: non-recursive `rm` of a directory fails on
both (PowerShell guards this explicitly); removing write-protected files succeeds
on PowerShell (`-Force` avoids interactive prompts) but fails on Bash; `mv` onto
itself succeeds on PowerShell; copying device files such as `/dev/null` works on
Bash but not through `Copy-Item`.

## Conditional compilation

```ts
// @cfg(windows)
import {} from "tsh:windows";

// #if unix && shell("bash")
echo("Unix Bash target");
// #elif windows
echo("Windows target");
// #else
echo("Other target");
// #endif
```

Predicates include `windows`, `linux`, `macos`, `freebsd`, `unix`, and `shell(...)`.
`shell("bash")` is true for `--target bash`; `shell("pwsh")` (or
`shell("powershell")`) is true for `--target pwsh`. Use `!`, `&&`, `||`, and
parentheses to combine them. `@cfg` applies to the following statement. Block
directives nest. Inactive source is blanked before parsing and module
resolution, preserving byte positions for diagnostics; excluded imports are
never opened or validated. The compilation target and `--os` determine
predicate values and built-in module compatibility (for example,
`tsh:windows` is rejected unless the target OS is Windows, regardless of the
shell target). No runtime conditional branches are generated for
compile-time conditions.

## Local modules

```ts
// utils.tsh
export function greeting(name: string): string {
  return `Hello, ${name}!`;
}
```

```ts
// main.tsh
import { greeting } from "./utils.tsh";
echo(greeting("Bash"));
```

Local modules resolve relative to their importer and are bundled into the final
standalone script. Cycles are rejected. Use named exports and imports with matching
names; default imports, namespace imports, and aliases are outside the initial
subset. Modules are not loaded at runtime.

## Raw shell and comments

```ts
raw!(`
trap 'printf "%s\n" "Cleaning up"' EXIT
printf '%s\n' 'Native Bash works!'
`);
```

`raw!()` accepts only a string literal or a template with no substitutions. It
injects shell text at that statement's position, including inside functions and
methods. The compiler does not interpret, execute, or syntax-check raw contents.
Dynamic arguments and plain `raw()` calls are errors. Guard backend-specific raw
blocks with `@cfg(shell("bash"))` or `@cfg(shell("pwsh"))`:

```ts
// @cfg(shell("pwsh"))
raw!(`
Write-Host 'Native PowerShell'
`);
```

`--comments all` preserves ordinary and documentation comments as shell comments
(`#` in both backends), including doc comments on functions, classes, and class
methods; `doc` keeps documentation comments; `none` omits source comments.
Compiler directives are omitted in every mode. Comments and text inside raw
blocks always remain unchanged.

## Rust API and compiler structure

```rust
use tsh::{compile_source, CompileOptions};

let script = compile_source(
    "example.tsh",
    "echo('Hello from Bash');",
    &CompileOptions::default(),
)?;
# Ok::<(), tsh::CompileError>(())
```

`compile_file` resolves local modules and emits shell source. `check_file` returns
the validated IR without executing it. Errors carry named source text and spans
for Miette rendering. Compiler state is owned by each compilation.

The pipeline is conditional preprocessing → Oxc TypeScript AST → module resolution
→ custom semantic validation → typed IR → backend code generation
(Bash or PowerShell, selected by `--target`). The backend consumes only the IR,
never Oxc nodes. The PowerShell emitter lives in `src/backends/powershell/`
(`mod.rs`, `emitter.rs`) behind the same `backends::Backend` trait. Parsing and
language semantics stay shared; platform-specific operations belong in module
metadata and target emission.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo run -- examples/hello.tsh
cargo run -- examples/counter.tsh
cargo run -- examples/imports.tsh
cargo run -- examples/powershell.tsh --target pwsh
```

Tests execute standalone generated scripts with Bash, including regressions for
subshell state loss, quoting, recursive returns, separate instance identity,
conditional imports, comments, CLI arguments, stdin, and exit status.
`tests/powershell.rs` mirrors that coverage for the PowerShell backend and
executes generated `.ps1` scripts with `pwsh` when installed (compiler-only
assertions always run), plus cross-backend tests that compare Bash and
PowerShell output for equivalent programs.

This is an experimental compiler. The supported subset is intentionally smaller
than TypeScript and raw shell is responsible for its own correctness. See the
integration tests and examples for executable language contracts.
