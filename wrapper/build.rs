use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest_dir.parent().unwrap();
    let ffmpeg_src = repo_root.join("ffmpeg");
    let dav1d_src = repo_root.join("dav1d");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let preinstalled_ffmpeg = env::var("FFMPEG_INSTALL_DIR");
    let ffmpeg_install = if preinstalled_ffmpeg.is_ok() {
        PathBuf::from(preinstalled_ffmpeg.unwrap())
    } else {
        out_dir.join("ffmpeg_install")
    };
    let dav1d_install = out_dir.join("dav1d_install");

    // Only rebuild if key build inputs change.
    println!(
        "cargo:rerun-if-changed={}",
        ffmpeg_src.join("configure").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dav1d_src.join("meson.build").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        repo_root.join("scripts/build_ffmpeg.sh").display()
    );
    println!("cargo:rerun-if-changed=build.rs");

    // Build dav1d + FFmpeg via the shared shell script (unless already cached).
    if !ffmpeg_install.join("lib").join("libavcodec.a").exists() {
        let build_script = repo_root.join("scripts/build_ffmpeg.sh");
        let target = env::var("TARGET").unwrap();

        let status = Command::new("bash")
            .arg(&build_script)
            .arg("--target")
            .arg(&target)
            .arg("--install-dir")
            .arg(&ffmpeg_install)
            .arg("--ffmpeg-src")
            .arg(&ffmpeg_src)
            .arg("--dav1d-src")
            .arg(&dav1d_src)
            .arg("--dav1d-install-dir")
            .arg(&dav1d_install)
            .status()
            .expect("Failed to run scripts/build_ffmpeg.sh — is bash available?");
        if !status.success() {
            panic!("scripts/build_ffmpeg.sh failed");
        }
    }

    // Link order matters: avcodec depends on avutil and swscale depends on avutil.
    // dav1d is pulled in by avcodec.
    let lib_dir = ffmpeg_install.join("lib");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!(
        "cargo:rustc-link-search=native={}",
        dav1d_install.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=avcodec");
    println!("cargo:rustc-link-lib=static=swscale");
    println!("cargo:rustc-link-lib=static=avutil");
    println!("cargo:rustc-link-lib=static=dav1d");

    // Platform-specific system libraries needed by FFmpeg.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    match target_os.as_str() {
        "linux" | "android" => {
            // println!("cargo:rustc-link-lib=pthread");
            // println!("cargo:rustc-link-lib=m");
        }
        "macos" | "ios" => {
            // println!("cargo:rustc-link-lib=pthread");
            // println!("cargo:rustc-link-lib=m");
        }
        "windows" => {
            println!("cargo:rustc-link-lib=bcrypt");
        }
        _ => {}
    }

    // Generate Rust FFI bindings via bindgen.
    let include_dir = ffmpeg_install.join("include");
    let bindings = bindgen::Builder::default()
        .header(include_dir.join("libavcodec/avcodec.h").to_str().unwrap())
        .header(include_dir.join("libavutil/frame.h").to_str().unwrap())
        .header(include_dir.join("libavutil/imgutils.h").to_str().unwrap())
        .header(include_dir.join("libavutil/pixfmt.h").to_str().unwrap())
        .header(include_dir.join("libswscale/swscale.h").to_str().unwrap())
        .clang_arg(format!("-I{}", include_dir.display()))
        .allowlist_function("av_.*")
        .allowlist_function("avcodec_.*")
        .allowlist_function("sws_.*")
        .allowlist_type("AV.*")
        .allowlist_type("Sws.*")
        .allowlist_var("AV_.*")
        .allowlist_var("FF_.*")
        .allowlist_var("SWS_.*")
        .prepend_enum_name(false)
        .generate_comments(false)
        .layout_tests(false)
        .generate()
        .expect("Failed to generate FFmpeg bindings");

    bindings
        .write_to_file(out_dir.join("ffmpeg_bindings.rs"))
        .expect("Failed to write bindings");
}
