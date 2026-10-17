mod winfsp {
    include!("../../build/winfsp.rs");
    pub fn configure() {
        main();
    }
}

fn main() {
    winfsp::configure();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
        && std::env::var_os("CARGO_FEATURE_MACOS_FUSE_T").is_some()
    {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/local/lib");
        let library = pkg_config::Config::new()
            .cargo_metadata(false)
            .probe("fuse")
            .expect("FUSE-T requires pkg-config; build with make FEATURES=macos-fuse-t");
        assert!(
            library.libs.iter().any(|name| name == "fuse-t"),
            "macos-fuse-t requires the FUSE-T library; build with make FEATURES=macos-fuse-t"
        );
    }
}
