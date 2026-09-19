fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("GifGun agent runtime");
    if let Err(error) = runtime.block_on(gifgun_agent::cli::run()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
