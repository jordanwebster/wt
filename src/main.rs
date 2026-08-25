mod app;
mod cli;

fn main() {
    if let Some(exit) = wt_sys::proc::owned_command_fast_path() {
        std::process::exit(exit);
    }
    std::process::exit(app::main(cli::parse()));
}
