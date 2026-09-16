//! Builds the vendored `libghostty-vt` with Zig and links it statically.
//!
//! Derived from herdr's `build.rs` (https://github.com/rksm/herdr),
//! Copyright the herdr authors, licensed under the Apache License, Version 2.0.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Zig release the vendored source is known to build with.
const REQUIRED_ZIG: &str = "0.16.0";

/// Maps a Rust target triple to the Zig target it corresponds to.
///
/// Windows maps to the GNU ABI so Zig supplies its own bundled MinGW libc.
/// Upstream Ghostty marks `x86_64-windows-msvc` as not working, and MSVC
/// additionally requires an installed Visual Studio / Windows SDK.
fn zig_target(target: &str) -> &'static str {
    match target {
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        "x86_64-apple-darwin" => "x86_64-macos",
        "aarch64-apple-darwin" => "aarch64-macos",
        "x86_64-pc-windows-gnu" => "x86_64-windows-gnu",
        "aarch64-pc-windows-gnu" => "aarch64-windows-gnu",
        other => panic!(
            "unsupported target for the vendored libghostty-vt build: {other}\n\
             On Windows use the GNU ABI (x86_64-pc-windows-gnu): upstream Ghostty \
             marks x86_64-windows-msvc as not working."
        ),
    }
}

fn env_bool(name: &str) -> Option<bool> {
    match env::var(name) {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            other => panic!("invalid boolean value for {name}: {other}"),
        },
        Err(env::VarError::NotPresent) => None,
        Err(err) => panic!("failed to read {name}: {err}"),
    }
}

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set"));
    let vendored = manifest_dir.join("../../vendor/libghostty-vt");

    for path in [
        "build.rs",
        "../../vendor/libghostty-vt.vendor.json",
        "../../vendor/libghostty-vt/build.zig",
        "../../vendor/libghostty-vt/build.zig.zon",
        "../../vendor/libghostty-vt/include",
        "../../vendor/libghostty-vt/pkg",
        "../../vendor/libghostty-vt/src",
        "../../vendor/libghostty-vt/VERSION",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    for var in [
        "LIBGHOSTTY_VT_OPTIMIZE",
        "LIBGHOSTTY_VT_SIMD",
        "LIBGHOSTTY_VT_ZIG_SYSTEM_DIR",
        "ZIG",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    let version = fs::read_to_string(vendored.join("VERSION"))
        .expect("failed to read vendored libghostty-vt VERSION")
        .trim()
        .to_string();
    // Zig wants a bare semantic version; the vendored string carries a
    // `-main-+<commit>` suffix that it rejects.
    let semver = version
        .split(['-', '+'])
        .next()
        .expect("split always yields at least one element")
        .to_string();

    let optimize = env::var("LIBGHOSTTY_VT_OPTIMIZE").unwrap_or_else(|_| "ReleaseFast".into());
    let simd = env_bool("LIBGHOSTTY_VT_SIMD").unwrap_or(true);
    let target = env::var("TARGET").expect("TARGET is always set");
    let zig = env::var("ZIG").unwrap_or_else(|_| "zig".into());

    // Zig installs into `zig-out` inside the source tree by default. That
    // directory is shared by every target, so building for two of them (a host
    // test run plus a Windows cross-build, say) has each overwrite the other's
    // libghostty-vt.a. OUT_DIR is unique per target and per crate, so install
    // there instead and link from there.
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is always set"));
    let prefix = out_dir.join("libghostty-vt");

    let mut command = Command::new(&zig);
    command
        .arg("build")
        .arg("--prefix")
        .arg(&prefix)
        .arg("-Demit-lib-vt")
        .arg(format!("-Doptimize={optimize}"))
        .arg(format!("-Dsimd={simd}"))
        .arg(format!("-Dtarget={}", zig_target(&target)))
        .arg(format!("-Dversion-string={semver}"))
        .arg("-Demit-xcframework=false");

    // Lets an offline or hermetic build point Zig at a pre-fetched package set
    // instead of downloading dependencies into vendor/libghostty-vt/zig-pkg.
    if let Ok(system_dir) = env::var("LIBGHOSTTY_VT_ZIG_SYSTEM_DIR") {
        command.arg("--system").arg(system_dir);
    }

    let status = command
        .current_dir(&vendored)
        .status()
        .unwrap_or_else(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                panic!(
                    "zig executable not found (looked for {zig:?}).\n\
                     Building Dispatch requires Zig {REQUIRED_ZIG} to compile the \
                     vendored libghostty-vt terminal engine.\n\
                     Install it from https://ziglang.org/download/ (or `brew install zig`), \
                     or set the ZIG environment variable to its path."
                );
            }
            panic!("failed to execute `{zig} build` for the vendored libghostty-vt: {err}");
        });
    assert!(
        status.success(),
        "`{zig} build` failed for the vendored libghostty-vt: {status}\n\
         Dispatch requires Zig {REQUIRED_ZIG}; check `zig version`, or set ZIG to a \
         matching binary."
    );

    let lib_dir = prefix.join("lib");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    // Zig emits a static library and a shared library side by side, and names
    // them differently per platform.
    if target.contains("apple-darwin") {
        // Both are in one directory and a plain `-l ghostty-vt` resolves to
        // the dylib, which is then not found at run time. Name the archive.
        println!(
            "cargo:rustc-link-arg={}",
            lib_dir.join("libghostty-vt.a").display()
        );
    } else if target.contains("windows") {
        // On Windows the static library is `ghostty-vt-static.lib`;
        // `ghostty-vt.lib` is the import library for `ghostty-vt.dll`. Linking
        // `ghostty-vt` would pick the import library and require the DLL
        // alongside the binary at run time.
        println!("cargo:rustc-link-lib=static=ghostty-vt-static");
    } else {
        println!("cargo:rustc-link-lib=static=ghostty-vt");
    }
    println!("cargo:include={}", prefix.join("include").display());
}
