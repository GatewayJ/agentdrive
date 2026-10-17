mod winfsp {
    include!("../../build/winfsp.rs");
    pub fn configure() {
        main();
    }
}
fn main() {
    winfsp::configure();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/local/lib");
    }
}
