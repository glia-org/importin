use std::process::ExitCode;

const USAGE: &str = "usage: importin [serve | doctor [--anon] [--json] | --version]";

fn main() -> ExitCode {
    importin::log::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("serve") if args.len() <= 1 => match serve() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("importin: {e}");
                ExitCode::from(1)
            }
        },
        Some("--version") if args.len() == 1 => {
            println!("importin {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("doctor") if args[1..].iter().all(|a| a == "--anon" || a == "--json") => {
            eprintln!("importin: doctor not implemented");
            ExitCode::from(2)
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(64)
        }
    }
}

fn serve() -> anyhow::Result<()> {
    let config = importin::config::load()?;
    tokio::runtime::Runtime::new()?.block_on(importin::tools::serve(config))
}
