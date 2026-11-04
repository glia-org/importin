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
            doctor(&importin::doctor::Options {
                anon: args.iter().any(|a| a == "--anon"),
                json: args.iter().any(|a| a == "--json"),
                ..Default::default()
            })
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

/// The one command that writes its result to stdout. Exits 2 unless Full Disk Access is granted and
/// the schema resolved.
fn doctor(opts: &importin::doctor::Options) -> ExitCode {
    let config = match importin::config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("importin: {e}");
            return ExitCode::from(2);
        }
    };
    let report = importin::doctor::run(&config, opts);
    print!("{}", importin::doctor::render(&report, opts.json));
    if importin::doctor::healthy(&report) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}
