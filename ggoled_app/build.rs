use std::io;

fn main() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-search=framework=/System/Library/PrivateFrameworks");

    #[cfg(target_os = "windows")]
    winresource::WindowsResource::new()
        .set_icon("assets/ggoled.ico")
        .compile()?;
    Ok(())
}
