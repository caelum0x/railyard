#[tokio::main]
async fn main() {
    std::process::exit(switchyard_cli::main_entry().await);
}
