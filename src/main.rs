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
            runtime.block_on(gifgun_agent::console::wait_for_close(
                tokio::signal::ctrl_c(),
            ));
        }
        std::process::exit(1);
    }
}
