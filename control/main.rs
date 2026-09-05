use clap::Parser;
use std::{ffi::CString, io::IsTerminal, os::unix::ffi::OsStrExt};
mod core;
mod docker;
mod headless;
mod kubeconfig;
mod kubernetes;
mod model;
mod service;

unsafe extern "C" {
    fn hamn_core_main(argc: libc::c_int, argv: *mut *mut libc::c_char) -> libc::c_int;
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("__core-worker") {
        std::process::exit(core::worker());
    }
    let mode = std::env::args().nth(1).unwrap_or_default();
    if matches!(
        mode.as_str(),
        "vmrun" | "port-observer" | "udp-forward" | "mount-inotify-watch" | "qcow2-extract"
    ) {
        std::process::exit(internal());
    }
    let mut request = match model::Request::try_parse() {
        Ok(request) => request,
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                print!("{error}");
                return;
            }
            println!(
                "{}",
                model::envelope(
                    &model::Request::default(),
                    "parse",
                    Err(model::Failure::new("invalidRequest", error))
                )
            );
            std::process::exit(2);
        }
    };
    if let Err(error) = request.normalize() {
        println!("{}", model::envelope(&request, "parse", Err(error)));
        std::process::exit(2);
    }
    if !request.headless {
        let message = if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
            "use --headless and an operation"
        } else {
            "a terminal is required; use hamn --headless <operation>"
        };
        println!(
            "{}",
            model::envelope(
                &request,
                "mode",
                Err(model::Failure::new("terminalRequired", message))
            )
        );
        std::process::exit(2);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("async runtime");
    std::process::exit(runtime.block_on(headless::run(request)));
}

fn internal() -> i32 {
    // C internal modes run before any Rust background thread is created.
    let args: Vec<CString> = std::env::args_os()
        .map(|arg| CString::new(arg.as_bytes()).expect("NUL in process argument"))
        .collect();
    let mut pointers: Vec<_> = args.iter().map(|arg| arg.as_ptr().cast_mut()).collect();
    pointers.push(std::ptr::null_mut());
    // C borrows argv for the duration of this call and never frees it.
    unsafe { hamn_core_main(args.len() as libc::c_int, pointers.as_mut_ptr()) }
}
