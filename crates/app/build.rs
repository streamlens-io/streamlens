//! Embeds the icon resource into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/streamlens.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/streamlens.ico");
        res.compile().expect("embed Windows icon resource");
    }
}
