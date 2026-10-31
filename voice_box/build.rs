use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use image::codecs::ico::{IcoEncoder, IcoFrame};
use resvg::{tiny_skia, usvg};

fn generate_icons(out: &Path) {
    let svg = fs::read("../assets/icon.svg").expect("Failed to read icon.svg");
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
    fs::write(
        out.join("voice_box.rc"),
        "1 ICON \"icon.ico\"\n1 24 \"voice_box.manifest\"\n",
    )
    .expect("Failed to write resource script");
    fs::copy(
        "../assets/voice_box.manifest",
        out.join("voice_box.manifest"),
    )
    .expect("Failed to copy application manifest");
}

fn compress_apo_dll(out: &Path) {
    let input = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../target")
        .join(env::var_os("PROFILE").unwrap())
        .join("voice_box_apo.dll");
    let output = out.join("voice_box_apo.dll.lz4");
    println!("cargo:rerun-if-changed={}", input.display());

    let dll = fs::read(&input).unwrap_or_else(|error| {
        panic!(
            "Failed to read APO DLL at {}: {error}. Build with ./build.ps1 first.",
            input.display()
        )
    });
    let compressed = lz4_flex::block::compress_prepend_size(&dll);
    fs::write(output, compressed).expect("Failed to write compressed APO DLL");
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../assets/icon.svg");
    println!("cargo:rerun-if-changed=../assets/voice_box.manifest");

    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    generate_icons(&out);
    compress_apo_dll(&out);

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
    if target.ends_with("msvc") {
        println!("cargo:rustc-link-arg-bin=voice_box=/MANIFEST:NO");
    }
}
