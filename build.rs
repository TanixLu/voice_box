use std::{env, fs, path::Path, path::PathBuf, process::Command};

use image::codecs::ico::{IcoEncoder, IcoFrame};
use resvg::{tiny_skia, usvg};

fn generate_icons(out: &Path) {
    let svg = fs::read("assets/icon.svg").expect("Failed to read icon.svg");
    let tree =
        usvg::Tree::from_data(&svg, &usvg::Options::default()).expect("Failed to parse icon.svg");
    let render_png = |size: u32| {
        let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("Failed to allocate icon image");
        let scale = size as f32 / tree.size().width().max(tree.size().height());
        let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(
            (size as f32 - tree.size().width() * scale) / 2.0,
            (size as f32 - tree.size().height() * scale) / 2.0,
        );
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        pixmap.encode_png().expect("Failed to encode icon PNG")
    };

    fs::write(out.join("icon.png"), render_png(256)).expect("Failed to write icon.png");
    let frames: Vec<_> = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
        .into_iter()
        .map(|size| {
            IcoFrame::with_encoded(
                render_png(size),
                size,
                size,
                image::ExtendedColorType::Rgba8,
            )
            .expect("Failed to create icon frame")
        })
        .collect();
    let ico = fs::File::create(out.join("icon.ico")).expect("Failed to create icon.ico");
    IcoEncoder::new(ico)
        .encode_images(&frames)
        .expect("Failed to encode icon.ico");
    fs::write(out.join("voice_box.rc"), "1 ICON \"icon.ico\"\n")
        .expect("Failed to write icon resource script");
}

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.svg");

    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    generate_icons(&out);

    let target = env::var("TARGET").unwrap();
    if !target.contains("windows") {
        return;
    }
    let resource = if target.ends_with("msvc") {
        let resource = out.join("voice_box.res");
        let mut compiler =
            find_msvc_tools::find(&target, "rc.exe").unwrap_or_else(|| Command::new("rc.exe"));
        let status = compiler
            .current_dir(&out)
            .arg("/nologo")
            .arg("/fo")
            .arg(&resource)
            .arg("voice_box.rc")
            .status()
            .expect("Windows SDK resource compiler (rc.exe) is required");
        assert!(status.success(), "Failed to compile the VoiceBox icon");
        resource
    } else {
        let resource = out.join("voice_box.o");
        let status = Command::new("windres")
            .current_dir(&out)
            .args(["-i", "voice_box.rc", "-o"])
            .arg(&resource)
            .status()
            .expect("MinGW resource compiler (windres) is required");
        assert!(status.success(), "Failed to compile the VoiceBox icon");
        resource
    };
    println!("cargo:rustc-link-arg-bin=voice_box={}", resource.display());
}
