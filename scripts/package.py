#!/usr/bin/env python3
"""Check packaging prerequisites; create a package only with explicit --build."""
import argparse, hashlib, json, os, platform, re, shutil, subprocess, tempfile, tomllib
from pathlib import Path
PROJECT = Path(__file__).resolve().parent.parent
STORAGE = PROJECT.parent.parent
CARGO = Path.home() / ".local/bin/cargo"


BUNDLED_THEMES = ("pastelduck", "pixelduck", "crt-full")

def validate_themes(root=None):
    """Fail before compiling if a stock theme or a declared local resource is missing."""
    root = Path(root) if root is not None else PROJECT / "themes"
    for name in BUNDLED_THEMES:
        folder = root / name
        config = json.loads((folder / "skin.json").read_text())
        def visit(value):
            if isinstance(value, dict):
                for key, item in value.items():
                    if key in ("file", "license") and isinstance(item, str):
                        path = folder / item
                        if not path.resolve().is_relative_to(folder.resolve()) or not path.is_file():
                            raise RuntimeError(f"Incomplete bundled theme {name}: {item}")
                    else:
                        visit(item)
            elif isinstance(value, list):
                for item in value:
                    visit(item)
        visit(config)
    return root

def bundle_themes(destination):
    source = validate_themes()
    shutil.copytree(source, destination)
    (destination / ".bundled-themes.json").write_text(json.dumps({"themes": BUNDLED_THEMES}, indent=2) + "\n")


def bundle_gstreamer(sdk, stage):
    """Copy selected plugins and their DLL import closure, never the complete SDK."""
    plugin_dir = sdk / "lib/gstreamer-1.0"
    output = stage / "lib/gstreamer-1.0"
    output.mkdir(parents=True)
    selected = ["coreelements", "playback", "typefindfunctions", "autodetect", "audioconvert", "audioresample", "volume", "isomp4", "matroska", "mpegtsdemux", "videoconvertscale", "videoparsersbad", "audioparsers", "mediafoundation", "libav", "opus", "vpx", "dav1d", "soup", "wasapi", "d3d12", "adaptivedemux2", "app", "openh264", "ogg", "vorbis", "flac", "deinterlace", "videofilter", "rawparse"]
    roots = list(stage.glob("*.exe"))
    for name in selected:
        matches = list(plugin_dir.glob("*gst" + name + ".dll"))
        if not matches: raise RuntimeError("Required Windows plugin missing: " + name)
        target = output / matches[0].name
        shutil.copy2(matches[0], target)
        roots.append(target)
    scanner = sdk / "libexec/gstreamer-1.0/gst-plugin-scanner.exe"
    if scanner.exists():
        target = stage / "libexec/gstreamer-1.0/gst-plugin-scanner.exe"
        target.parent.mkdir(parents=True)
        shutil.copy2(scanner, target)
        roots.append(target)
    gio = sdk / "lib/gio/modules"
    if gio.is_dir():
        shutil.copytree(gio, stage / "lib/gio/modules")
        roots.extend((stage / "lib/gio/modules").glob("*.dll"))
    cert = sdk / "etc/ssl/certs/ca-certificates.crt"
    if cert.is_file():
        destination = stage / "etc/ssl/certs/ca-certificates.crt"
        destination.parent.mkdir(parents=True)
        shutil.copy2(cert, destination)
    available = {p.name.lower(): p for p in (sdk / "bin").glob("*.dll")}
    available.update({p.name.lower(): p for p in (STORAGE / "windows/vc-runtime/extracted/System64").glob("*.dll")})
    system = set("avicap32 uiautomationcore bcryptprimitives combase d2d1 rstrtmgr kernel32 user32 gdi32 gdi32full opengl32 shell32 ole32 oleaut32 ws2_32 winmm advapi32 crypt32 bcrypt ntdll msvcrt ucrtbase version shlwapi shcore comdlg32 comctl32 imm32 setupapi secur32 dnsapi iphlpapi winhttp wininet propsys psapi dwmapi dwrite d3d11 d3d12 d3dcompiler_47 dxgi dxva2 mf mfplat mfreadwrite mfuuid ksuser avrt powrprof userenv wintrust ncrypt cfgmgr32 dbghelp normaliz usp10 rpcrt4 hid winspool msimg32 mswsock msasn1 win32u winusb dcomp wtsapi32 authz netapi32 uxtheme".split())
    visited = set()
    unresolved = set()
    while roots:
        binary = roots.pop()
        imports = subprocess.check_output([str(STORAGE / "tools/bin/llvm-readobj"), "--coff-imports", str(binary)], text=True)
        for name in re.findall(r"Name: (\S+\.dll)", imports, re.IGNORECASE):
            key = name.lower()
            if key in visited: continue
            visited.add(key)
            if key in available:
                target = stage / available[key].name
                shutil.copy2(available[key], target)
                roots.append(target)
            elif key.removesuffix(".dll") not in system and not key.startswith(("api-ms-", "ext-ms-")):
                unresolved.add(name)
    if unresolved: raise RuntimeError("Unresolved Windows runtime DLLs: " + ", ".join(sorted(unresolved)))


def bundle_rust_licenses(destination, target):
    """Ship dependency notices on both platforms, including REUSE LICENSES dirs."""
    metadata = json.loads(subprocess.check_output([str(CARGO), "metadata", "--offline", "--locked", "--format-version", "1", "--filter-platform", target, "--manifest-path", str(PROJECT / "Cargo.toml")], text=True))
    destination.mkdir(parents=True, exist_ok=True)
    notices = []
    for package in metadata["packages"]:
        if package["name"] == "mochune": continue
        source = Path(package["manifest_path"]).parent
        component = destination / "rust" / (package["name"] + "-" + package["version"])
        notices.append(package["name"] + " " + package["version"] + ": " + (package.get("license") or "See included license") + " " + (package.get("repository") or ""))
        candidates = {path for path in source.iterdir() if path.name.upper().startswith(("LICENSE", "COPYING", "NOTICE"))}
        if package.get("license_file"):
            candidates.add(source / package["license_file"])
        for license in sorted(candidates):
            if not license.exists(): continue
            component.mkdir(parents=True, exist_ok=True)
            if license.is_dir():
                shutil.copytree(license, component / license.name, dirs_exist_ok=True)
            elif license.is_file():
                shutil.copy2(license, component / license.name)
    (destination / "RUST-DEPENDENCIES.txt").write_text("\n".join(sorted(notices)) + "\n")


def debian_dependencies(executables):
    """Use host package metadata for the actual linked-library ABI requirements."""
    with tempfile.TemporaryDirectory(dir=STORAGE / "tmp", prefix="deb-shlibs-") as tmp:
        root = Path(tmp)
        (root / "debian").mkdir()
        (root / "debian/control").write_text("Source: mochune\nSection: video\nPriority: optional\nMaintainer: Local build <local@localhost>\n\nPackage: mochune\nArchitecture: any\nDescription: Mochune native player\n")
        output = subprocess.check_output(["dpkg-shlibdeps", "--ignore-missing-info", "-O", *("-e" + str(p) for p in executables)], cwd=root, text=True)
        for line in output.splitlines():
            if line.startswith("shlibs:Depends="):
                return line.split("=", 1)[1]
        raise RuntimeError("No shared-library dependency metadata returned")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=["deb", "windows", "windows-installer"])
    parser.add_argument("--build", action="store_true")
    args = parser.parse_args()
    windows = args.target.startswith("windows")
    installer = args.target == "windows-installer"
    missing = []
    if not CARGO.exists(): missing.append("configured Rust launcher")
    if args.target in ("deb", "windows", "windows-installer"):
        helper_dir = STORAGE / ("windows/helpers" if windows else "tools/bin")
        suffix = ".exe" if windows else ""
        for name in ("yt-dlp", "deno"):
            helper_path = helper_dir / (name + suffix)
            if windows and name == "yt-dlp":
                helper_path = helper_dir / "yt-dlp/yt-dlp.exe"
                if not (helper_dir / "yt-dlp/_internal/python310.dll").is_file():
                    missing.append("unpacked yt-dlp runtime (run scripts/prepare-windows-helper.py)")
            if not helper_path.is_file(): missing.append(name + " bundled helper")
    if windows and not (helper_dir / "ffmpeg.exe").is_file(): missing.append("FFmpeg download merger")
    if args.target == "deb":
        if platform.system() != "Linux": missing.append("Linux Debian build host")
        if not shutil.which("dpkg-deb"): missing.append("dpkg-deb")
        if not shutil.which("dpkg-shlibdeps"): missing.append("dpkg-shlibdeps")
        pkg_env=dict(os.environ, PKG_CONFIG_PATH=os.environ.get("PKG_CONFIG_PATH_x86_64_unknown_linux_gnu", str(STORAGE / "tools/gstreamer-sdk/usr/lib/x86_64-linux-gnu/pkgconfig")))
        if not shutil.which("pkg-config") or subprocess.run(["pkg-config", "--exists", "gstreamer-1.0 >= 1.24", "gstreamer-gl-1.0", "gstreamer-app-1.0"], env=pkg_env, check=False).returncode: missing.append("GStreamer development libraries")
    elif windows:
        gst_sdk=Path(os.environ.get("GSTREAMER_1_0_ROOT_MSVC_X86_64",str(STORAGE / "windows/gstreamer/msvc_x86_64")))
        for path,label in [(STORAGE / "cargo-home/bin/cargo-xwin", "cargo-xwin"), (STORAGE / "tools/bin/clang-cl", "external clang-cl"), (STORAGE / "tools/bin/lld-link", "external lld-link"), (gst_sdk / "lib/gstreamer-1.0.lib", "GStreamer MSVC x64 SDK"), (gst_sdk / "lib/gstreamer-1.0", "GStreamer Windows plugins")]:
            if not path.exists(): missing.append(label)
        for path,label in [(gst_sdk / "lib/gstd3d12-1.0.lib", "GStreamer 1.26 D3D12 library"), (gst_sdk / "lib/gstreamer-1.0/gstd3d12.dll", "GStreamer D3D12 plugin")]:
            if not path.is_file(): missing.append(label)
        if not list((STORAGE / "windows/xwin").glob("**/kernel32.lib")): missing.append("Windows SDK cache")
        if not (STORAGE / "tools/windows-packaging/root/usr/bin/makensis").is_file(): missing.append("portable executable compiler (NSIS)")
        if not (STORAGE / "windows/vc-runtime/extracted/System64/vcruntime140.dll").is_file(): missing.append("Microsoft app-local VC runtime")
    print(json.dumps({"target": args.target, "ready": not missing, "missing": missing, "package_created": False}, indent=2))
    if missing: raise SystemExit(1)
    if not args.build: return
    validate_themes()
    subprocess.run([str(Path.home() / ".local/bin/rust-external"), "--paths"], check=True)
    if windows:
        win_env=dict(os.environ, PKG_CONFIG_ALLOW_CROSS="1", PKG_CONFIG_PATH_x86_64_pc_windows_msvc=str(gst_sdk / "lib/pkgconfig"))
        subprocess.run([str(CARGO), "xwin", "build", "--locked", "--release", "--target", "x86_64-pc-windows-msvc", "--manifest-path", str(PROJECT / "Cargo.toml"), "-j", "2"], env=win_env, check=True)
        packages = STORAGE / "packages"
        packages.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=STORAGE / "tmp", prefix="windows-") as stage:
            stage = Path(stage)
            shutil.copy2(STORAGE / "target/x86_64-pc-windows-msvc/release/mochune.exe", stage / "mochune.exe")
            for name in ("deno.exe", "ffmpeg.exe"): shutil.copy2(helper_dir / name, stage / name)
            shutil.copytree(helper_dir / "yt-dlp", stage / "yt-dlp")
            bundle_gstreamer(gst_sdk, stage)
            bundle_themes(stage / "themes")
            shutil.copy2(PROJECT / "README.md", stage / "README.md")
            shutil.copytree(PROJECT / "third-party", stage / "licenses")
            shutil.copy2(PROJECT / "LICENSE", stage / "LICENSE")
            shutil.copy2(PROJECT / "NOTICE", stage / "NOTICE")
            shutil.copy2(STORAGE / "windows/vc-runtime/LICENSE.rtf", stage / "licenses/Microsoft-VC-Runtime.rtf")
            sdk_licenses = gst_sdk / "share/licenses"
            if sdk_licenses.is_dir():
                shutil.copytree(sdk_licenses, stage / "licenses/gstreamer")
            bundle_rust_licenses(stage / "licenses", "x86_64-pc-windows-msvc")
            (stage / "PORTABLE.txt").write_text("Mochune portable Windows x64. No installation or administrator rights required.\nThe launcher creates Mochune beside itself. Runtime files are in Mochune/runtime; settings, playlists, caches and scratch files are in Mochune/data.\nLater launches reuse the runtime. Updates replace only runtime, preserving data.\nClose the app and delete the entire Mochune folder to remove this portable copy and all its data. The launcher can recreate a fresh copy on its next launch.\nData from older LocalAppData builds is not automatically imported or deleted.\nWindows playback and GPU behavior still need device testing.\n")
            identity = hashlib.sha256()
            for file in sorted(p for p in stage.rglob("*") if p.is_file()):
                identity.update(file.relative_to(stage).as_posix().encode() + b"\0")
                with file.open("rb") as source:
                    identity.update(hashlib.file_digest(source, "sha256").digest())
            runtime_id = identity.hexdigest()
            nsis = STORAGE / "tools/windows-packaging/root/usr"
            output = packages / ("Mochune-Windows-x64-setup.exe" if installer else "Mochune-Windows-x64-portable.exe")
            pending = output.with_name(output.stem + ".building.exe")
            nsis_env = dict(os.environ, NSISDIR=str(nsis / "share/nsis"), LD_LIBRARY_PATH=str(nsis / "lib/x86_64-linux-gnu"))
            version = tomllib.loads((PROJECT / "Cargo.toml").read_text())["package"]["version"]
            script = "windows-installer.nsi" if installer else "windows-portable.nsi"
            subprocess.run([str(nsis / "bin/makensis"), "-V2", "-DOUTPUT=" + str(pending), "-DPAYLOAD=" + str(stage), "-DRUNTIME_ID=" + runtime_id, "-DVERSION=" + version, str(PROJECT / "packaging" / script)], env=nsis_env, check=True)
            os.replace(pending, output)
            with output.open("rb") as artifact:
                digest = hashlib.file_digest(artifact, "sha256").hexdigest()
            output.with_suffix(output.suffix + ".sha256").write_text(digest + "  " + output.name + "\n")
            report = {"output": str(output), "exe_bytes": output.stat().st_size, "runtime_bytes": sum(p.stat().st_size for p in stage.rglob("*") if p.is_file()), "runtime_id": runtime_id, "persistent_folder": "Mochune", "data_folder": "Mochune/data", "windows_runtime_tested": False}
            report["kind"] = "per-user-installer" if installer else "portable"
            if installer:
                report.update(persistent_folder="%LOCALAPPDATA%/Programs/Mochune", data_folder="%LOCALAPPDATA%/Mochune")
            (packages / ("windows-installer-build-report.json" if installer else "windows-build-report.json")).write_text(json.dumps(report, indent=2) + "\n")
            print(json.dumps(report, indent=2))
        return
    subprocess.run([str(CARGO), "build", "--locked", "--release", "-j", "2", "--manifest-path", str(PROJECT / "Cargo.toml")], check=True)
    version = tomllib.loads((PROJECT / "Cargo.toml").read_text())["package"]["version"]
    arch = subprocess.check_output(["dpkg", "--print-architecture"], text=True).strip()
    packages = STORAGE / "packages"
    packages.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=STORAGE / "tmp", prefix="deb-") as stage:
        stage = Path(stage)
        (stage / "DEBIAN").mkdir()
        (stage / "usr/bin").mkdir(parents=True)
        shutil.copy2(STORAGE / "target/release/mochune", stage / "usr/bin/mochune")
        (stage / "usr/bin/mochune").chmod(0o755)
        shutil.copytree(PROJECT / "third-party", stage / "usr/share/doc/mochune/licenses")
        shutil.copy2(PROJECT / "LICENSE", stage / "usr/share/doc/mochune/licenses/Mochune-GPL-3.0.txt")
        shutil.copy2(PROJECT / "NOTICE", stage / "usr/share/doc/mochune/copyright")
        bundle_themes(stage / "usr/share/mochune/themes")
        bundle_rust_licenses(stage / "usr/share/doc/mochune/licenses", "x86_64-unknown-linux-gnu")
        (stage / "usr/lib/mochune").mkdir(parents=True)
        for name in ("yt-dlp", "deno"):
            shutil.copy2(helper_dir / name, stage / "usr/lib/mochune" / name)
            (stage / "usr/lib/mochune" / name).chmod(0o755)
        runtime_deps = debian_dependencies([stage / "usr/bin/mochune", *(stage / "usr/lib/mochune").iterdir()])
        dependencies = runtime_deps + ", util-linux, ffmpeg, libgstreamer1.0-0 (>= 1.24), libgstreamer-gl1.0-0, gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-gl, gstreamer1.0-libav, libfontconfig1, libxkbcommon0, libxkbcommon-x11-0, libx11-xcb1, libxcursor1, libx11-6, libegl1, libgl1"
        icon_dir=stage / "usr/share/icons/hicolor/128x128/apps"
        icon_dir.mkdir(parents=True)
        shutil.copy2(PROJECT / "ui/assets/logo-128.png",icon_dir / "mochune.png")
        applications=stage / "usr/share/applications"
        applications.mkdir(parents=True)
        (applications / "mochune.desktop").write_text("[Desktop Entry]\nType=Application\nName=Mochune\nExec=mochune\nIcon=mochune\nStartupWMClass=mochune\nTerminal=false\nCategories=AudioVideo;Player;\n")
        cleanup = (PROJECT / "packaging/debian-clean-profile.sh").read_text()
        prerm = (PROJECT / "packaging/debian-prerm.in").read_text().replace("@PROFILE_CLEANUP@", cleanup)
        (stage / "DEBIAN/prerm").write_text(prerm)
        (stage / "DEBIAN/prerm").chmod(0o755)
        (stage / "DEBIAN/control").write_text(f"Package: mochune\nVersion: {version}\nArchitecture: {arch}\nMaintainer: Local build <local@localhost>\nDepends: {dependencies}\nDescription: Mochune native YouTube browser and player\n")
        output = packages / f"mochune_{version}_{arch}.deb"
        # TemporaryDirectory starts at 0700; never ship that mode as the archive root.
        stage.chmod(0o755)
        for path in stage.rglob("*"):
            path.chmod(0o755 if path.is_dir() or path.stat().st_mode & 0o111 else 0o644)
        pending = output.with_name(output.stem + ".building.deb")
        subprocess.run(["dpkg-deb", "--root-owner-group", "--build", str(stage), str(pending)], check=True)
        os.replace(pending, output)
        with output.open("rb") as artifact:
            digest = hashlib.file_digest(artifact, "sha256").hexdigest()
        output.with_suffix(output.suffix + ".sha256").write_text(digest + "  " + output.name + "\n")
        print(output)


if __name__ == "__main__": main()
