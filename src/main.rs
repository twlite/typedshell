mod cli;
mod updater;

fn main() {
    match cli::execute() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error:?}");
            std::process::exit(1);
        }
    }
}
