//! Embed `assets/icon.ico` as the exe's file icon on Windows builds.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        winresource::WindowsResource::new()
            .set_icon("../../assets/icon.ico")
            .compile()
            .expect("embedding the exe icon failed");
    }
}
