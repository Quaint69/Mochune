#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    let result = run();
    #[cfg(windows)]
    if let Err(error) = &result {
        mochune_native::diagnostics_event(
            "fatal",
            serde_json::json!({"error":format!("{error:#}")}),
        );
        if std::env::args().any(|a| a == "--video-worker") {
            return result;
        }
        // The portable GUI has no console; make startup/playback setup errors visible.
        let message: Vec<u16> = format!("Mochune could not start:\n{error:#}")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let title: Vec<u16> = "Mochune".encode_utf16().chain(Some(0)).collect();
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBoxW(
                window: *mut std::ffi::c_void,
                text: *const u16,
                title: *const u16,
                kind: u32,
            ) -> i32;
        }
        // SAFETY: null owner and valid, NUL-terminated strings for this synchronous call.
        unsafe {
            MessageBoxW(std::ptr::null_mut(), message.as_ptr(), title.as_ptr(), 0x10);
        }
    }
    result
}

fn run() -> anyhow::Result<()> {
    mochune_native::identity::initialize()?;
    #[cfg(target_os = "windows")]
    use anyhow::Context;
    #[cfg(target_os = "windows")]
    configure_bundled_runtime()?;
    #[cfg(target_os = "windows")]
    set_windows_app_identity();
    #[cfg(target_os = "windows")]
    {
        let data = std::env::var_os("MOCHUNE_DATA_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| directories::BaseDirs::new().map(|d| d.data_local_dir().join("Mochune")));
        if let Some(data) = data {
            let log_data = if std::env::args().any(|a| a == "--video-worker") {
                data.join("video-worker")
            } else {
                data
            };
            mochune_native::init_diagnostics(&log_data);
        }
        let old = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            mochune_native::diagnostics_event(
                "panic",
                serde_json::json!({"detail":info.to_string()}),
            );
            old(info);
        }));
    }
    let args: Vec<String> = std::env::args().collect();
    #[cfg(target_os="windows")]
    if let Some(index)=args.iter().position(|a|a=="--probe-windows-interpolation") {
        let result=mochune_native::check_interpolation_shaders().and_then(|_|mochune_native::probe_windows_interpolation());
        let report=match result{Ok(v)=>v,Err(e)=>serde_json::json!({"ok":false,"error":format!("{e:#}")})};
        if let Some(path)=args.get(index+1){std::fs::write(path,serde_json::to_vec_pretty(&report)?)?;}
        return Ok(());
    }

    if let Some(index) = args.iter().position(|a| a == "--preview-theme") {
        let path = args
            .get(index + 1)
            .ok_or_else(|| anyhow::anyhow!("Usage: --preview-theme /absolute/path/skin.json"))?;
        mochune_native::validate_music_theme(std::path::Path::new(path))?;
        mochune_native::run_ui(false)?;
        return Ok(());
    }
    if let Some(index) = args.iter().position(|a| a == "--validate-theme") {
        let path = args
            .get(index + 1)
            .ok_or_else(|| anyhow::anyhow!("Usage: --validate-theme /absolute/path/skin.json"))?;
        println!(
            "{}",
            serde_json::to_string_pretty(&mochune_native::validate_music_theme(
                std::path::Path::new(path)
            )?)?
        );
        return Ok(());
    }
    #[cfg(feature = "native-overlay-test")]
    if args.iter().any(|a| a == "--probe-music-skins") {
        println!("{}", mochune_native::probe_music_skins()?);
        return Ok(());
    }
    #[cfg(feature = "native-overlay-test")]
    if args.iter().any(|a| a == "--probe-native-overlay") {
        println!("{}", mochune_native::probe_native_overlay()?);
        return Ok(());
    }
    #[cfg(feature = "native-overlay-test")]
    if args.iter().any(|a| a == "--probe-main-layout") {
        println!("{}", mochune_native::probe_main_layout()?);
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    if args.iter().any(|a| a == "--video-worker") {
        let parent = args
            .iter()
            .position(|a| a == "--parent-window")
            .and_then(|i| args.get(i + 1))
            .context("Missing video host")?
            .parse()?;
        return mochune_native::run_video_worker(parent);
    }
    if args.iter().any(|arg| arg == "--doctor") {
        println!("{}", mochune_native::doctor()?);
    } else if let Some(index) = args.iter().position(|arg| arg == "--probe-recommendations") {
        let id = args
            .get(index + 1)
            .map(String::as_str)
            .unwrap_or("aqz-KE-bpKQ");
        println!("{}", mochune_native::probe_recommendations(id)?);
    } else if let Some(index) = args.iter().position(|arg| arg == "--probe-youtube") {
        let query = args
            .get(index + 1)
            .map(String::as_str)
            .unwrap_or("Blender animation");
        println!("{}", mochune_native::probe_youtube(query)?);
    } else {
        mochune_native::run_ui(args.iter().any(|arg| arg == "--smoke-ui"))?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn configure_bundled_runtime() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let root = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Missing executable directory"))?;
    if !root.join("lib/gstreamer-1.0").is_dir() {
        return Ok(());
    }
    // Portable state lives beside the runtime. Installed state belongs to the
    // current user's LocalAppData, including plugin registries and scratch files.
    let packaged_data = if root.join("PORTABLE.txt").is_file() {
        Some(root.parent().unwrap_or(root).join("data"))
    } else if root.join("INSTALLED.txt").is_file() {
        Some(
            directories::BaseDirs::new()
                .ok_or_else(|| anyhow::anyhow!("Missing user data directory"))?
                .data_local_dir()
                .join("Mochune"),
        )
    } else {
        None
    };
    if let Some(data) = &packaged_data {
        std::fs::create_dir_all(data.join("tmp"))?;
        std::fs::create_dir_all(data.join("runtime-cache"))?;
        // Fail visibly rather than silently saving to a different user folder.
        let probe = data.join(".write-check");
        std::fs::write(&probe, [])?;
        std::fs::remove_file(probe)?;
    }
    // SAFETY: this runs before any application threads or multimedia initialization.
    unsafe {
        if let Some(data) = &packaged_data {
            std::env::set_var("MOCHUNE_DATA_DIR", data);
            for name in ["TMP", "TEMP", "TMPDIR"] {
                std::env::set_var(name, data.join("tmp"));
            }
            std::env::set_var("XDG_CACHE_HOME", data.join("runtime-cache"));
            let bundle = std::fs::read_to_string(root.join(".bundle-id"))
                .unwrap_or_else(|_| "unversioned-runtime".into());
            std::env::set_var("MOCHUNE_RUNTIME_ID", bundle.trim());
            let registry = refresh_plugin_registry(&data.join("runtime-cache"), bundle.trim())?;
            std::env::set_var("GST_REGISTRY_1_0", registry);
        }
        std::env::set_var("GST_PLUGIN_PATH_1_0", root.join("lib/gstreamer-1.0"));
        std::env::set_var("GST_PLUGIN_SYSTEM_PATH_1_0", root.join("lib/gstreamer-1.0"));
        std::env::set_var(
            "GST_PLUGIN_SCANNER_1_0",
            root.join("libexec/gstreamer-1.0/gst-plugin-scanner.exe"),
        );
        std::env::set_var("GIO_MODULE_DIR", root.join("lib/gio/modules"));
        if std::env::var_os("SSL_CERT_FILE").is_none() {
            std::env::set_var(
                "SSL_CERT_FILE",
                root.join("etc/ssl/certs/ca-certificates.crt"),
            );
        }
        let mut paths = vec![root.to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::set_var("PATH", std::env::join_paths(paths)?);
    }
    Ok(())
}

// A blacklisted plugin is not necessarily rescanned when only its DLL dependency changes.
// Keep a single cache, but invalidate it whenever the portable payload changes.
#[cfg(any(target_os = "windows", test))]
fn refresh_plugin_registry(
    cache: &std::path::Path,
    bundle: &str,
) -> anyhow::Result<std::path::PathBuf> {
    let registry = cache.join("gstreamer-registry.bin");
    let marker = cache.join("gstreamer-bundle-id");
    if std::fs::read_to_string(&marker).ok().as_deref() != Some(bundle) {
        match std::fs::remove_file(&registry) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        std::fs::write(marker, bundle)?;
    }
    Ok(registry)
}
#[cfg(test)]
mod registry_tests {
    #[test]
    fn runtime_update_rescans_previously_blacklisted_plugins() {
        let cache = std::env::temp_dir().join(format!("mochune-registry-{}", std::process::id()));
        std::fs::create_dir_all(&cache).unwrap();
        let registry = cache.join("gstreamer-registry.bin");
        std::fs::write(&registry, b"old blacklist").unwrap();
        super::refresh_plugin_registry(&cache, "bundle-a").unwrap();
        assert!(!registry.exists());
        std::fs::write(&registry, b"valid cache").unwrap();
        super::refresh_plugin_registry(&cache, "bundle-a").unwrap();
        assert!(registry.exists());
        super::refresh_plugin_registry(&cache, "bundle-b").unwrap();
        assert!(!registry.exists());
        std::fs::remove_dir_all(cache).unwrap();
    }
}

#[cfg(target_os = "windows")]
fn set_windows_app_identity() {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SetCurrentProcessExplicitAppUserModelID(id: *const u16) -> i32;
    }
    let id: Vec<u16> = "Quaint.Mochune".encode_utf16().chain(Some(0)).collect();
    // Both the desktop and same-executable playback helper share this identity.
    unsafe {
        SetCurrentProcessExplicitAppUserModelID(id.as_ptr());
    }
}
