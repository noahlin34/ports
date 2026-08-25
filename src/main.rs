mod app;
mod cli;
mod help;
mod theme;
mod tui;

use clap::Parser;

fn main() {
    let command_line = cli::Cli::parse();
    if command_line.command.is_none() {
        if let Err(error) = tui::run() {
            eprintln!("ports: {error:#}");
            std::process::exit(1);
        }
        return;
    }

    if let Err(error) = cli::run(command_line) {
        eprintln!("ports: {error}");
        std::process::exit(error.exit_code());
    }
}
