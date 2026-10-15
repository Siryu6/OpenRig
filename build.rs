fn main() {
    slint_build::compile("ui/app.slint").unwrap();

    #[cfg(feature = "nam")]
    build_nam();
}

/// Build NeuralAudio and the C wrapper in native/ as static libraries.
#[cfg(feature = "nam")]
fn build_nam() {
    let neural_audio = std::path::Path::new("third_party/NeuralAudio/NeuralAudio/CMakeLists.txt");
    if !neural_audio.exists() {
        panic!(
            "third_party/NeuralAudio is missing: run `git submodule update --init --recursive`, \
             or build without the `nam` feature (--no-default-features --features winit,lv2,tone3000,midi)"
        );
    }

    // Always optimized: a debug build of the model cannot keep up in real time.
    // The optimization level is explicit: Buildroot's CMake toolchain file
    // sets the Release flags to -DNDEBUG alone and puts -O2 in
    // CMAKE_CXX_FLAGS, which the cmake crate replaces, leaving the model at -O0
    let out = cmake::Config::new("native")
        .profile("Release")
        .define("CMAKE_C_FLAGS_RELEASE", "-O3 -DNDEBUG")
        .define("CMAKE_CXX_FLAGS_RELEASE", "-O3 -DNDEBUG")
        .build_target("pedalboard_nam")
        .build();

    println!(
        "cargo:rustc-link-search=native={}",
        out.join("build/lib").display()
    );
    println!("cargo:rustc-link-lib=static=pedalboard_nam");
    println!("cargo:rustc-link-lib=static=RTNeural");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target == "macos" {
        println!("cargo:rustc-link-lib=dylib=c++");
    } else if target != "windows" {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-changed=third_party/NeuralAudio/NeuralAudio");
}
