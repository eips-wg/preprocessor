fn main() {
    println!("cargo::rerun-if-changed=Cargo.toml"); // Automatically rebuild for clap.
}
