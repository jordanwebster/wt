mod app;
mod cli;

fn main() {
    std::process::exit(app::main(cli::parse()));
}
