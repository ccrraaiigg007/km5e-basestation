// build.rs — embed the app icon into the Windows .exe resource table.
// Runs only on Windows targets; a no-op on other platforms.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }

    // Re-run this script if any icon asset changes.
    println!("cargo:rerun-if-changed=assets/icon_32.png");
    println!("cargo:rerun-if-changed=assets/icon_64.png");
    println!("cargo:rerun-if-changed=assets/icon_256.png");

    // Build a multi-resolution ICO in the Cargo output directory.
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let ico_path = std::path::Path::new(&out_dir).join("icon.ico");

    // Collect each size as raw RGBA bytes.
    let sizes = ["assets/icon_32.png", "assets/icon_64.png", "assets/icon_256.png"];
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);

    for path in &sizes {
        let img = image::open(path)
            .unwrap_or_else(|e| panic!("Failed to open {}: {}", path, e))
            .into_rgba8();
        let (w, h) = img.dimensions();
        let rgba = img.into_raw();
        let icon_image = ico::IconImage::from_rgba_data(w, h, rgba);
        icon_dir.add_entry(ico::IconDirEntry::encode(&icon_image)
            .unwrap_or_else(|e| panic!("Failed to encode {}: {}", path, e)));
    }

    let ico_file = std::fs::File::create(&ico_path)
        .expect("Failed to create icon.ico in OUT_DIR");
    icon_dir.write(ico_file).expect("Failed to write icon.ico");

    // Embed the ICO into the exe via a Windows resource script.
    let mut res = winres::WindowsResource::new();
    res.set_icon(ico_path.to_str().unwrap());
    res.compile().expect("Failed to compile Windows resources");
}
