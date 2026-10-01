fn main() {
    if let Err(error) = lumvise_mcp_app_adapter::run_lumvise(std::env::args().skip(1).collect()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
