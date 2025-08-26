fn main() {
    // This is only necessary for Windows targets
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        embed_resource::compile("build.rc", embed_resource::NONE);
    }
}
