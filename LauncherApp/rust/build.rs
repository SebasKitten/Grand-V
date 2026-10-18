fn main() {
    println!("cargo:rerun-if-changed=assets/app.png");
    println!("cargo:rerun-if-changed=build.rs");
    let source = image::open("assets/app.png")
        .expect("Read GrandV logo")
        .into_rgba8();
    // Fit visible artwork, rather than the source image's transparent export canvas.
    let (mut left, mut top, mut right, mut bottom) = (source.width(), source.height(), 0, 0);
    for (x, y, pixel) in source.enumerate_pixels() {
        if pixel[3] > 8 {
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
    }
    assert!(right > left && bottom > top, "Logo has no visible artwork");
    let cropped =
        image::imageops::crop_imm(&source, left, top, right - left, bottom - top).to_image();
    let edge = cropped.width().max(cropped.height());
    let padding = (edge / 50).max(1);
    let side = edge + 2 * padding;
    let mut original = image::RgbaImage::new(side, side);
    image::imageops::replace(
        &mut original,
        &cropped,
        ((side - cropped.width()) / 2) as i64,
        ((side - cropped.height()) / 2) as i64,
    );
    let output_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    original
        .save(output_dir.join("app-icon.png"))
        .expect("Write fitted app icon");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut icons = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16, 24, 32, 48, 64, 128, 256] {
        let resized =
            image::imageops::resize(&original, size, size, image::imageops::FilterType::Lanczos3);
        let icon = ico::IconImage::from_rgba_data(size, size, resized.into_raw());
        icons.add_entry(ico::IconDirEntry::encode(&icon).expect("Encode app icon"));
    }
    let output = output_dir.join("app.ico");
    icons
        .write(std::fs::File::create(&output).expect("Create app ICO"))
        .expect("Write app ICO");
    winresource::WindowsResource::new()
        .set_icon(output.to_str().unwrap())
        .set("ProductName", "GrandV Back end launcher")
        .set("FileDescription", "GrandV Back end launcher")
        .set("InternalName", "GrandV Back end launcher")
        .set("OriginalFilename", "GrandV Back end launcher.exe")
        .compile()
        .expect("Embed GrandV icon and version resource");
}
