use std::io::IsTerminal;

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("GifGun agent runtime");
    if let Err(error) = runtime.block_on(gifgun_agent::cli::run()) {
        eprintln!("{error}");
        let arguments = std::env::args_os().collect::<Vec<_>>();
        let console = arguments.len() == 1
            || arguments.get(1).and_then(|value| value.to_str()) == Some("launch");
        if console && std::io::stdin().is_terminal() {
            println!("Press Enter to close this console.");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
        std::process::exit(1);
    }
}
