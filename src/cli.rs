use clap::{Parser, Subcommand};
use std::{
    ffi::OsString,
    path::PathBuf,
    time::{Duration, Instant},
};
use tsh::{Comments, CompileOptions, Target, TargetOs};

#[derive(Parser)]
#[command(
    name = "tsh",
    version,
    disable_version_flag = true,
    about = "Compile a typed TypeScript subset to standalone shell scripts"
)]
struct Cli {
    /// Print version information
    #[arg(short = 'v', long, action = clap::ArgAction::Version)]
    _version: Option<bool>,
    #[command(subcommand)]
    command: Option<Action>,
    /// Source to compile and immediately execute
    script: Option<PathBuf>,
    #[command(flatten)]
    options: Options,
    /// Print generated shell source instead of writing or executing it
    #[arg(long, global = true)]
    print: bool,
    /// Replace this executable with the latest GitHub release
    #[arg(long, global = true)]
    upgrade: bool,
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
        /// Print timings for compilation stages
        #[arg(short, long)]
        verbose: bool,
    },
    /// Validate syntax, imports, and static types
    Check {
        source: PathBuf,
        #[command(flatten)]
        options: Options,
        /// Print timings for validation stages
        #[arg(short, long)]
        verbose: bool,
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

fn report_stage_timing(verbose: bool, stage: tsh::compiler::CompileStage, elapsed: Duration) {
    if !verbose {
        return;
    }
    let label = match stage {
        tsh::compiler::CompileStage::Resolve => "resolve imports",
        tsh::compiler::CompileStage::Analyze => "semantic analysis",
        tsh::compiler::CompileStage::Emit => "backend emission",
    };
    report_timing(label, elapsed);
}

fn report_timing(label: &str, elapsed: Duration) {
    eprintln!(
        "verbose: {label}: {:.3} ms",
        elapsed.as_secs_f64() * 1_000.0
    );
}

pub fn execute() -> miette::Result<i32> {
    let cli = Cli::parse();
    if cli.upgrade {
        if cli.command.is_some() || cli.script.is_some() || cli.print || !cli.args.is_empty() {
            return Err(miette::miette!("--upgrade must be used by itself"));
        }
        return crate::updater::upgrade();
    }

    match cli.command {
        Some(Action::Compile {
            source,
            options,
            output,
            verbose,
        }) => {
            if cli.print && output.is_some() {
                return Err(miette::miette!("--print cannot be combined with --output"));
            }
            let compile_options = options.compile_options();
            let compile_started = Instant::now();
            let mut observer = |stage, elapsed| report_stage_timing(verbose, stage, elapsed);
            let generated =
                tsh::compiler::compile_file_with_observer(&source, &compile_options, &mut observer);
            if verbose {
                report_timing("compile total", compile_started.elapsed());
            }
            let generated = generated?;
            if cli.print {
                print!("{generated}");
                return Ok(0);
            }
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
            let write_started = Instant::now();
            let write_result = std::fs::write(&output, generated);
            if verbose {
                report_timing("write output", write_started.elapsed());
            }
            write_result.map_err(|e| miette::miette!("cannot write {}: {e}", output.display()))?;
            Ok(0)
        }
        Some(Action::Check {
            source,
            options,
            verbose,
        }) => {
            if cli.print {
                return Err(miette::miette!("--print cannot be used with `check`"));
            }
            let check_started = Instant::now();
            let mut observer = |stage, elapsed| report_stage_timing(verbose, stage, elapsed);
            let checked = tsh::compiler::check_file_with_observer(
                source,
                &options.compile_options(),
                &mut observer,
            );
            if verbose {
                report_timing("check total", check_started.elapsed());
            }
            checked?;
            Ok(0)
        }
        None => {
            let source = cli.script.ok_or_else(|| {
                miette::miette!("provide a source file or a compile/check command")
            })?;
            let compile_options = cli.options.compile_options();
            let generated = tsh::compile_file(source, &compile_options)?;
            if cli.print {
                if !cli.args.is_empty() {
                    return Err(miette::miette!(
                        "--print cannot be combined with script arguments"
                    ));
                }
                print!("{generated}");
                return Ok(0);
            }
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
