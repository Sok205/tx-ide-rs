fn main() {
    std::process::exit(tx::run(std::env::args_os().skip(1).collect()));
}
