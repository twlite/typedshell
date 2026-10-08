use clap::{Parser, Subcommand};
use std::{ffi::OsString, path::PathBuf};
use tsh::{Comments, CompileOptions, Target, TargetOs};

#[derive(Parser)]
#[command(
    name = "tsh",
    version,
    about = "Compile a typed TypeScript subset to standalone shell scripts"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Action>,
    /// Source to compile and immediately execute
    script: Option<PathBuf>,
    #[command(flatten)]
    options: Options,
    /// Arguments forwarded unchanged to the script after --
    #[arg(last = true)]
    args: Vec<OsString>,
}
#[derive(Subcommand)]
enum Action {
    /// Compile a standalone shell script
    Compile {
        source: PathBuf,
        #[command(flatten)]
        options: Options,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Validate syntax, imports, and static types
    Check {
        source: PathBuf,
        #[command(flatten)]
        options: Options,
    },
}
#[derive(clap::Args)]
struct Options {
    #[arg(long, value_enum, default_value = "bash")]
    target: Target,
    #[arg(long, value_enum)]
    os: Option<TargetOs>,
    #[arg(long, value_enum, default_value = "all")]
    comments: Comments,
}
impl Options {
    fn compile_options(self) -> CompileOptions {
        CompileOptions {
            target: self.target,
            os: self.os.unwrap_or_else(TargetOs::host),
            comments: self.comments,
        }
    }
}
pub fn execute() -> miette::Result<i32> {
    let cli = Cli::parse();
    match cli.command {
        Some(Action::Compile {
            source,
            options,
            output,
        }) => {
            let compile_options = options.compile_options();
            let generated = tsh::compile_file(&source, &compile_options)?;
            let default_extension = match compile_options.target {
                Target::Bash => "sh",
                Target::Pwsh => "ps1",
            };
            let output = output.unwrap_or_else(|| source.with_extension(default_extension));
            if std::fs::canonicalize(&output)
                .ok()
                .is_some_and(|p| std::fs::canonicalize(&source).ok().as_ref() == Some(&p))
            {
                return Err(miette::miette!(
                    "output must not overwrite the input source"
                ));
            }
            std::fs::write(&output, generated)
                .map_err(|e| miette::miette!("cannot write {}: {e}", output.display()))?;
            Ok(0)
        }
        Some(Action::Check { source, options }) => {
            tsh::check_file(source, &options.compile_options())?;
            Ok(0)
        }
        None => {
            let source = cli.script.ok_or_else(|| {
                miette::miette!("provide a source file or a compile/check command")
            })?;
            let compile_options = cli.options.compile_options();
            let generated = tsh::compile_file(source, &compile_options)?;
            let status = match compile_options.target {
                Target::Bash => tsh::runner::run(&generated, &cli.args).map_err(|e| miette::miette!("cannot execute Bash: {e}"))?,
                Target::Pwsh => tsh::runner::run_pwsh(&generated, &cli.args).map_err(|e| miette::miette!("cannot execute PowerShell: install `pwsh` (PowerShell 7.4 or newer) to run `--target pwsh` scripts: {e}"))?,
            };
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                Ok(status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
            }
            #[cfg(not(unix))]
            {
                Ok(status.code().unwrap_or(1))
            }
        }
    }
}
