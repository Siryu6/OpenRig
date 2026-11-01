//! Build-time only: failures stop the build with their message

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result {
    // Without a display backend (tests, headless checks) Slint has no image
    // decoder for embedded images: embed them decoded instead
    let headless = std::env::var_os("CARGO_FEATURE_WINIT").is_none()
        && std::env::var_os("CARGO_FEATURE_KMS").is_none();
    let mut config = slint_build::CompilerConfiguration::new();
    if headless {
        config = config.embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer);
    }
    slint_build::compile_with_config("ui/app.slint", config)?;

    #[cfg(feature = "nam")]
    build_nam()?;
    Ok(())
}

/// Build NeuralAudio and the C wrapper in native/ as static libraries.
#[cfg(feature = "nam")]
fn build_nam() -> Result {
    let neural_audio = std::path::Path::new("third_party/NeuralAudio/NeuralAudio/CMakeLists.txt");
    if !neural_audio.exists() {
        return Err(
            "third_party/NeuralAudio is missing: run `git submodule update --init --recursive`, \
             or build without the `nam` feature (--no-default-features --features winit,lv2,tone3000,midi)"
                .into(),
        );
    }

    // Always optimized: a debug build of the model cannot keep up in real time.
    // The optimization level is explicit: Buildroot's CMake toolchain file
    // sets the Release flags to -DNDEBUG alone and puts -O2 in
    // CMAKE_CXX_FLAGS, which the cmake crate replaces, leaving the model at -O0
    // PEDALBOARD_NAM_CFLAGS tunes the model for a known CPU (an OS image),
    // e.g. -mcpu=cortex-a76: generic builds run on any CPU of the target
    let tuning = std::env::var("PEDALBOARD_NAM_CFLAGS").unwrap_or_default();
    println!("cargo:rerun-if-env-changed=PEDALBOARD_NAM_CFLAGS");
    let release = format!("-O3 -DNDEBUG {tuning}");
    let out = cmake::Config::new("native")
        .profile("Release")
        .define("CMAKE_C_FLAGS_RELEASE", release.trim())
        .define("CMAKE_CXX_FLAGS_RELEASE", release.trim())
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
    Ok(())
}
