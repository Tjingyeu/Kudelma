mod cli;
mod cmd;
mod headless;

use std::process::ExitCode;

use clap::Parser;

#[tokio::main]
async fn main() -> ExitCode {
    color_eyre::install().ok();
    match cmd::dispatch(cli::Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("\n\x1b[1;31m✖ {error}\x1b[0m");
            let mut causes = error.chain().skip(1);
            if let Some(cause) = causes.next() {
                eprintln!("\x1b[2m  {cause}\x1b[0m");
            }
            ExitCode::FAILURE
        }
    }
}
