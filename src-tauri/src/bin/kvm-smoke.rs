fn main() {
    if let Err(error) = kvm_client::smoke::run() {
        eprintln!("kvm-smoke: {error}");
        std::process::exit(1);
    }
}
