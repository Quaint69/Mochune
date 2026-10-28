fn main() {
    println!("cargo:rerun-if-changed=packaging/app.rc");
    println!("cargo:rerun-if-changed=ui/assets/app.ico");
    println!("cargo:rerun-if-env-changed=MOCHUNE_LLVM_RC");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let compiler = std::env::var_os("MOCHUNE_LLVM_RC")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("RUST_EXTERNAL_ROOT")
                    .map(|p| std::path::PathBuf::from(p).join("tools/bin/llvm-rc"))
            })
            .unwrap_or_else(|| "llvm-rc".into());
        let resource = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap())
            .join("mochune-icon.res");
        let status = std::process::Command::new(compiler)
            .args(["/no-preprocess", "/FO"])
            .arg(&resource)
            .arg("packaging/app.rc")
            .status()
            .expect("Windows icon resource compiler unavailable (set MOCHUNE_LLVM_RC)");
        assert!(status.success(), "Windows icon resource compilation failed");
        println!("cargo:rustc-link-arg-bin=mochune={}", resource.display());
    }

    // Explicit contexts keep the offscreen software controls independent of
    // the native desktop window. The compiler and runtime are pinned to 1.18.1.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        || std::env::var_os("CARGO_FEATURE_NATIVE_OVERLAY_TEST").is_some()
    {
        // This build script is single-threaded, before Slint compilation starts.
        unsafe {
            std::env::set_var("SLINT_ENABLE_EXPERIMENTAL_FEATURES", "1");
        }
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        || std::env::var_os("CARGO_FEATURE_NATIVE_OVERLAY_TEST").is_some()
    {
        let overlay_config = slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
        slint_build::compile_with_config("ui/native-player.slint", overlay_config)
            .expect("Native controls UI failed to compile");
        println!(
            "cargo:rustc-env=MOCHUNE_NATIVE_UI={}",
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap())
                .join("native-player.rs")
                .display()
        );
    }
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/setup.slint", config)
        .expect("Slint setup UI failed to compile");
}
