use std::{error::Error, path::PathBuf};

use tsh::{CompileOptions, compile_file};

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("examples/hello.tsh"));
    let script = compile_file(path, &CompileOptions::default())?;
    print!("{script}");
    Ok(())
}
