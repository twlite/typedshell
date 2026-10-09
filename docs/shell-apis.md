# Shell APIs

TypedShell includes a small set of operations that compile to native shell
commands or APIs. They are available as global calls in source. Values passed
to regular APIs are quoted by the backend; normal strings are not evaluated as
shell code.

## Commands

| Call | Behavior |
| --- | --- |
| `echo(value, ...)` | Write one or more scalar values followed by a newline. |
| `mkdir(path, { recursive: true })` | Create directories; recursive mode also creates parents. |
| `rm(path, { recursive: true })` | Remove files or recursively remove a directory tree. |
| `cp(source, destination)` | Copy a file. |
| `mv(source, destination)` | Move a path. |
| `cd(path)` | Change the script's working directory. |
| `run(command, [args...])` | Run a native command with explicit argument boundaries. |
| `exit(code)` | Exit with a numeric status; the status defaults to zero. |

Filesystem paths and command arguments are passed as literal values. Where a
Unix `printf` command is available, this does not expand the wildcard or
interpret the file name as shell syntax:

```ts
const file = "a file with spaces.txt";
run("printf", ["<%s>\\n", file]);
```

`run()` requires a command string and a literal array of string arguments. The
array is special syntax for this API; general array values are not implemented.
`mkdir()` and `rm()` accept a literal options object with the `recursive`
boolean field. A failing native command exits the generated script with the
command's status.

## Environment and host information

`env(name)` reads an environment variable and returns an empty string when it is
unset. `setEnv(name, value)` and `TypedShell.setEnv(name, value)` set a
process-level environment variable for the script and its child processes.

```ts
setEnv("TSH_MODE", "build");
echo(env("TSH_MODE"));

echo(TypedShell.platform()); // linux, macos, windows, or freebsd
echo(TypedShell.arch());     // normalized architecture, such as x86_64
echo(TypedShell.homedir());
```

`TypedShell.addPath(path)` resolves the path to an absolute path, prepends it to
the current process PATH once, and updates the user's persistent PATH or shell
profile. Use it when an installation script intentionally changes the user's
environment. PATH separators cannot occur inside an added path.

## Unix permissions

Import `chmod` from `tsh:unix` to set a file mode on Unix targets:

```ts
import { chmod } from "tsh:unix";

chmod("./tool", 0o755);
```

Numeric modes are supported on Bash and PowerShell Unix targets. String modes
such as `"u+x"` are supported by the Bash command but rejected by the
PowerShell backend; use a numeric mode for code shared across targets. `chmod`
is unavailable for the Windows OS target.

## Raw shell

Use `raw!()` when the program needs shell syntax that has no TypedShell API. It
accepts a string literal or a template literal without substitutions and must
appear as a standalone statement:

```ts
// @cfg(shell("bash"))
raw!(`
printf '%s\\n' 'Bash-specific behavior'
`);
```

Raw text is inserted without parsing, quoting, or validation. It can change
shell state and is responsible for its own error handling. Keep raw blocks
backend-specific with a `shell(...)` conditional. Ordinary values passed to
TypedShell operations remain quoted.

## Comments

The CLI option `--comments` controls preserved comments in generated scripts:

- `all` keeps ordinary and documentation comments.
- `doc` keeps documentation comments only.
- `none` omits source comments.

Compiler directives are omitted in every mode. Text inside raw blocks is kept
verbatim.
