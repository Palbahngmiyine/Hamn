use std::{env, path::PathBuf, process::Command};

fn main() {
    assert_eq!(env::var("CARGO_CFG_TARGET_OS").unwrap(), "macos");
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    // Nix toolchain wrappers can replace SDKROOT after the CI shell selects it.
    // The separately preserved selection must govern both native and Rust links.
    let sdk = env::var_os("HAMN_SYSTEM_SDKROOT")
        .filter(|value| !value.is_empty())
        .or_else(|| env::var_os("SDKROOT").filter(|value| !value.is_empty()))
        .map(PathBuf::from);
    if let Some(sdk) = &sdk {
        assert!(sdk.is_absolute() && sdk.is_dir(), "SDKROOT must name an absolute SDK directory");
        // The native make invocation uses -isysroot, but that flag does not
        // propagate to Rust's final cc invocation (notably inside Nix shells).
        println!("cargo:rustc-link-arg=-isysroot");
        println!("cargo:rustc-link-arg={}", sdk.display());
    }
    let version = env::var("HAMN_VERSION").unwrap_or_else(|_| "0.0.1".into());
    let native = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("core");
    let mut make = Command::new("make");
    make.current_dir(&root).args([
            format!("{}/libhamn_core.a", native.display()),
            format!("BUILD={}", native.display()),
            format!("VERSION={version}"),
        ]);
    if let Some(sdk) = sdk {
        make.arg(format!("SDKROOT={}", sdk.display()));
    }
    let status = make
        .status()
        .expect("make is required to build the C virtualization core");
    assert!(status.success(), "C core build failed");
    println!("cargo:rustc-link-search=native={}", native.display());
    println!("cargo:rustc-link-lib=static=hamn_core");
    for framework in ["Virtualization", "Foundation", "CoreServices"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    println!("cargo:rustc-link-lib=z");
    // Objective-C @available uses Apple's compiler-rt availability helpers.
    let runtime = Command::new("clang")
        .arg("--print-file-name=libclang_rt.osx.a")
        .output()
        .expect("cannot locate Apple compiler runtime");
    assert!(runtime.status.success());
    let runtime = PathBuf::from(String::from_utf8(runtime.stdout).unwrap().trim());
    assert!(runtime.is_file(), "Apple compiler runtime is missing");
    println!(
        "cargo:rustc-link-search=native={}",
        runtime.parent().unwrap().display()
    );
    println!("cargo:rustc-link-lib=static=clang_rt.osx");
    println!("cargo:rustc-env=HAMN_VERSION={version}");
    for path in [
        "host",
        "vendor",
        "Makefile",
        "scripts/embed-retirement.py",
        "guest/scripts",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    for key in ["HAMN_VERSION", "SDKROOT", "HAMN_SYSTEM_SDKROOT", "MACOSX_DEPLOYMENT_TARGET"] {
        println!("cargo:rerun-if-env-changed={key}");
    }
}
