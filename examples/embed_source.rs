use std::error::Error;

use tsh::{CompileOptions, compile_source};

fn main() -> Result<(), Box<dyn Error>> {
    let source = r#"
const name: string = "TypedShell";
echo(`Hello from ${name}`);
"#;

    let script = compile_source("embedded.tsh", source, &CompileOptions::default())?;
    print!("{script}");
    Ok(())
}
