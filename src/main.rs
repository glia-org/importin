use std::process::ExitCode;

const USAGE: &str = "usage: importin [serve | doctor [--anon] [--json] | --version]";

fn main() -> ExitCode {
    importin::log::init();
    importin::log::install_panic_hook();
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
    let channel = importin::tools::guard_stdout()?;
    if std::env::var_os("IMPORTIN_TEST_STRAY_PRINT").is_some() {
        println!("stray");
    }
    let config = importin::config::load()?;
    tokio::runtime::Runtime::new()?.block_on(importin::tools::serve(config, channel))
}
