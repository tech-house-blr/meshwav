// Windows only: embed the icon so Explorer shows it on meshwav.exe itself.
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../linux-receiver-gui/assets/meshwav.ico");
        res.set("FileDescription", "Meshwav");
        res.set("ProductName", "Meshwav");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
