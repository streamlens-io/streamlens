//! Generates FFI bindings for the vendored analysis decoders and links their
//! static libraries. Prefixes are produced by the patched builds in
//! vendor/ (see vendor/README): libde265 with the internals API, FFmpeg with
//! the SL H.264 analysis side data.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest.join("../../vendor");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // cfg!() in a build script reflects the host, not the target.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    // On windows-gnu, MSYS2's libclang disagrees with rustc about the
    // layout of FFmpeg structs the bindings never touch (AVHWAccel,
    // AVOption, …), failing the generated compile-time asserts. The decode
    // smoke tests cover real ABI breakage in the structs we do use.
    let layout_tests = target_os != "windows";

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
        ffmpeg.join("lib/libavformat.a"),
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
        .layout_tests(layout_tests)
        .derive_default(true)
        .generate()
        .expect("bindgen libde265")
        .write_to_file(out.join("de265_bindings.rs"))
        .expect("write de265 bindings");

    // FFmpeg: demux + decode API, and our analysis side data.
    bindgen::Builder::default()
        .header_contents(
            "ffmpeg_wrapper.h",
            "#include <libavformat/avformat.h>\n\
             #include <libavcodec/avcodec.h>\n\
             #include <libavutil/opt.h>\n\
             #include <libavutil/imgutils.h>\n\
             #include <libavutil/sl_h264_analysis.h>\n",
        )
        .clang_arg(format!("-I{}", ffmpeg.join("include").display()))
        .allowlist_function(
            "avformat_.*|av_read_frame|av_seek_frame|av_dict_.*|avio_.*|avcodec_.*|av_packet_.*|av_frame_.*|av_opt_set.*|av_log_set_level|av_image_.*|av_strerror",
        )
        .allowlist_type("SLH264.*|AVFrameSideDataType|AVPixelFormat|AVCodecID|AVMediaType|AVDiscard")
        .allowlist_var("AV_.*|AVFMT_.*|AVSEEK_.*|FF_.*|SL_H264.*")
        .layout_tests(layout_tests)
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
    println!("cargo:rustc-link-lib=static=avformat");
    println!("cargo:rustc-link-lib=static=avcodec");
    println!("cargo:rustc-link-lib=static=avutil");

    // FFmpeg is configured with --disable-autodetect: no zlib, and only the
    // TLS backend vendor/build-decoders.sh picks per OS. libde265 is C++.
    match target_os.as_str() {
        "macos" => {
            println!("cargo:rustc-link-lib=c++");
            println!("cargo:rustc-link-lib=m");
            // SecureTransport
            println!("cargo:rustc-link-lib=framework=Security");
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
            // libde265's x86 code calls __builtin_cpu_supports, whose
            // helpers (___cpu_model, ___cpu_indicator_init) live in clang's
            // runtime library; rustc links with -nodefaultlibs, so it has
            // to be named explicitly.
            let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
            let resource_dir = std::process::Command::new(&cc)
                .arg("-print-resource-dir")
                .output()
                .expect("run the C compiler to find clang's runtime library");
            let resource_dir = String::from_utf8_lossy(&resource_dir.stdout);
            println!(
                "cargo:rustc-link-search=native={}/lib/darwin",
                resource_dir.trim()
            );
            println!("cargo:rustc-link-lib=static=clang_rt.osx");
        }
        // MinGW (x86_64-pc-windows-gnu): no separate libm, and libavutil's
        // random_seed uses BCryptGenRandom.
        "windows" => {
            println!("cargo:rustc-link-lib=stdc++");
            println!("cargo:rustc-link-lib=bcrypt");
            // Winsock and Schannel
            for lib in ["ws2_32", "secur32", "ncrypt", "crypt32"] {
                println!("cargo:rustc-link-lib={lib}");
            }
        }
        _ => {
            println!("cargo:rustc-link-lib=stdc++");
            println!("cargo:rustc-link-lib=m");
            // mbedTLS, built statically by vendor/build-decoders.sh.
            let mbedtls = vendor.join("mbedtls-prefix/lib");
            println!("cargo:rustc-link-search=native={}", mbedtls.display());
            for lib in ["mbedtls", "mbedx509", "mbedcrypto"] {
                println!("cargo:rustc-link-lib=static={lib}");
                println!(
                    "cargo:rerun-if-changed={}",
                    mbedtls.join(format!("lib{lib}.a")).display()
                );
            }
        }
    }
}
