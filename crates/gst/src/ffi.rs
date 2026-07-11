//! Raw FFI bindings to the vendored decoders (generated at build time).

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::all
)]
pub mod de265 {
    include!(concat!(env!("OUT_DIR"), "/de265_bindings.rs"));
}

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::all
)]
pub mod ffmpeg {
    include!(concat!(env!("OUT_DIR"), "/ffmpeg_bindings.rs"));
}
