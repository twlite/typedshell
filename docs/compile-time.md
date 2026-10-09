# Compile-time selection

The output shell and the target operating system are separate compiler options:

```sh
tsh compile examples/powershell.tsh --target bash --os linux -o example.sh
tsh compile examples/powershell.tsh --target pwsh --os windows -o example.ps1
```

`--target` selects generated Bash or PowerShell. `--os` selects `linux`,
`macos`, `windows`, or `freebsd` for compile-time predicates and built-in module
compatibility. If omitted, the OS defaults to the host running the compiler.
The OS option does not make other platform commands or tools available at
runtime.

## Conditional source

Use `@cfg(predicate)` on its own line to include the following statement only
when the predicate matches:

```ts
// @cfg(windows)
import {} from "tsh:windows";

// @cfg(macos)
const installRoot = "/opt/typedshell";

// @cfg(linux)
const installRoot = "/usr/local/typedshell";
```

For larger sections, use block directives:

```ts
// #if shell("bash") && unix
echo("Bash on a Unix target");
// #elif windows
echo("Windows target");
// #else
echo("Other target");
// #endif
```

Predicates are `windows`, `linux`, `macos`, `freebsd`, `unix`, and
`shell("bash")`, `shell("pwsh")` (also spelled `shell("powershell")`). Combine
them with `!`, `&&`, `||`, and parentheses. `unix` matches Linux, macOS, and
FreeBSD. Block directives can nest; `@cfg` selects one following statement.

Inactive source is blanked before TypeScript parsing and module resolution. It
is not emitted into the script, and its imports are not opened or checked. This
allows a branch to contain syntax or modules that are only valid for its
selected target. Diagnostics retain their positions in the original source.

## Built-in modules

Built-in imports are checked against `--os` at compile time; they do not load
runtime modules. The shell operations themselves are listed in
[Shell APIs](shell-apis.md).

| Module | Target OS | Exports |
| --- | --- | --- |
| `tsh:fs` | All supported OS values | `echo`, `mkdir`, `rm`, `cp`, `mv`, `cd` |
| `tsh:process` | All supported OS values | `run`, `env`, `exit` |
| `tsh:unix` | Linux, macOS, FreeBSD | `chmod` |
| `tsh:windows` | Windows | No runtime exports; usable as a compatibility marker. |
| `tsh:linux` | Linux | No runtime exports; usable as a compatibility marker. |
| `tsh:macos` | macOS | No runtime exports; usable as a compatibility marker. |

An import that is incompatible with `--os` fails even when it imports no names.
Unknown exports fail at compile time. Platform markers are useful for making
compatibility requirements explicit:

```ts
// This file can be compiled only for the Windows OS target.
import {} from "tsh:windows";
```

## Local modules

Relative local imports use paths relative to the importing file, are checked
against named exports, and are bundled dependency-first into one standalone
script. Cyclic imports are rejected. The working directory used to invoke
`tsh` does not change how an import such as `"./utils.tsh"` is resolved.
