//! Generates FFI bindings for the vendored analysis decoders and links their
//! static libraries. Prefixes are produced by the patched builds in
//! vendor/ (see vendor/README): libde265 with the internals API, FFmpeg with
//! the SL H.264 analysis side data.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest.join("../../vendor");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let de265 = vendor.join("libde265-prefix");
    let ffmpeg = vendor.join("ffmpeg-prefix");

    for (prefix, header) in [
        (&de265, "include/libde265/de265_internals.h"),
        (&ffmpeg, "include/libavutil/sl_h264_analysis.h"),
    ] {
        assert!(
            prefix.join(header).exists(),
            "missing {}/{header} — build the vendored decoders first (see vendor/)",
            prefix.display()
        );
        println!("cargo:rerun-if-changed={}", prefix.join(header).display());
    }
    // Relink when the vendored static libraries are rebuilt.
    for lib in [
        de265.join("lib/libde265.a"),
        ffmpeg.join("lib/libavcodec.a"),
        ffmpeg.join("lib/libavutil.a"),
    ] {
        println!("cargo:rerun-if-changed={}", lib.display());
    }

    // libde265: decoder API + internals analytics API.
    bindgen::Builder::default()
        .header(
            de265
                .join("include/libde265/de265.h")
                .to_str()
                .unwrap()
                .to_owned(),
        )
        .header(
            de265
                .join("include/libde265/de265_internals.h")
                .to_str()
                .unwrap()
                .to_owned(),
        )
        .clang_arg(format!("-I{}", de265.join("include").display()))
        .allowlist_function("de265_.*")
        .allowlist_type("de265_.*")
        .allowlist_var("DE265_.*")
        .derive_default(true)
        .generate()
        .expect("bindgen libde265")
        .write_to_file(out.join("de265_bindings.rs"))
        .expect("write de265 bindings");

    // FFmpeg: minimal libavcodec decode API + our analysis side data.
    bindgen::Builder::default()
        .header_contents(
            "ffmpeg_wrapper.h",
            "#include <libavcodec/avcodec.h>\n\
             #include <libavutil/opt.h>\n\
             #include <libavutil/imgutils.h>\n\
             #include <libavutil/sl_h264_analysis.h>\n",
        )
        .clang_arg(format!("-I{}", ffmpeg.join("include").display()))
        .allowlist_function(
            "avcodec_.*|av_packet_.*|av_frame_.*|av_opt_set.*|av_log_set_level|av_image_.*",
        )
        .allowlist_type("SLH264.*|AVFrameSideDataType|AVPixelFormat|AVCodecID")
        .allowlist_var("AV_.*|FF_.*|SL_H264.*")
        .derive_default(true)
        .generate()
        .expect("bindgen ffmpeg")
        .write_to_file(out.join("ffmpeg_bindings.rs"))
        .expect("write ffmpeg bindings");

    println!(
        "cargo:rustc-link-search=native={}",
        de265.join("lib").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        ffmpeg.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=de265");
    println!("cargo:rustc-link-lib=static=avcodec");
    println!("cargo:rustc-link-lib=static=avutil");
    // libde265 is C++.
    println!("cargo:rustc-link-lib=c++");
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=CoreVideo");
        println!("cargo:rustc-link-lib=framework=CoreMedia");
        println!("cargo:rustc-link-lib=framework=VideoToolbox");
        println!("cargo:rustc-link-lib=framework=CoreServices");
    }
    println!("cargo:rustc-link-lib=z");
    println!("cargo:rustc-link-lib=m");
}
