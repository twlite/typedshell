# TypedShell guides

TypedShell (`tsh`) compiles a statically checked subset of TypeScript syntax to
standalone Bash or PowerShell scripts. It does not implement all of TypeScript;
the [language guide](language.md) lists the supported syntax and its limits.

## Guides

- [Language guide](language.md): values, expressions, control flow, functions,
  classes, and local modules.
- [Shell APIs](shell-apis.md): filesystem and process operations, environment
  access, host information, PATH setup, and raw shell blocks.
- [Compile-time selection](compile-time.md): target OS, shell targets,
  conditional compilation, and built-in modules.

The repository [README](../README.md) covers installation, CLI usage, Rust
embedding, release publishing, and development commands. Runnable programs are
in [`examples/`](../examples/).

## Try a feature example

From the repository root:

```sh
tsh check examples/counter.tsh
tsh compile examples/counter.tsh --print
tsh examples/counter.tsh
```

The compiler defaults to Bash for the host OS. Select another backend with
`--target pwsh`; compiling PowerShell does not require `pwsh`, but executing the
generated script does. See the [platform guide](compile-time.md) for target
selection details.
