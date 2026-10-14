//! Console twin of the GUI for CI: a Windows GUI-subsystem exe has no console, so the CLI is its own binary.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(powerwizz::cli::main(&args));
}
