extern crate cc;

fn main() {
    cc::Build::new()
        .flag("-std=c99")
        .flag("-Wall")
        .flag("-Wextra")
        .flag("-pedantic")
        .file("src/audio_stream.c")
        .file("src/media.c")
        .file("src/musicd.c")
        .compile("libmusicdc.a");

    println!("cargo:rustc-link-lib=dylib=pthread");
    println!("cargo:rustc-link-lib=dylib=avcodec");
    println!("cargo:rustc-link-lib=dylib=avfilter");
    println!("cargo:rustc-link-lib=dylib=avformat");
    println!("cargo:rustc-link-lib=dylib=avutil");
}
