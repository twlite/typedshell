use std::error::Error;

use tsh::{CompileError, CompileOptions, backends::Backend, check_file, ir::Program};

struct JsonSummaryBackend;

impl Backend for JsonSummaryBackend {
    fn emit(&self, program: &Program) -> Result<String, CompileError> {
        Ok(format!(
            "{{\n  \"functions\": {},\n  \"classes\": {},\n  \"top_level_statements\": {}\n}}\n",
            program.functions.len(),
            program.classes.len(),
            program.body.len(),
        ))
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let program = check_file("examples/hello.tsh", &CompileOptions::default())?;
    print!("{}", JsonSummaryBackend.emit(&program)?);
    Ok(())
}
