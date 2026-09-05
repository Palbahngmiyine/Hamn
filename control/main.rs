use std::{ffi::CString, os::unix::ffi::OsStrExt};

unsafe extern "C" {
    fn hamn_core_main(argc: libc::c_int, argv: *mut *mut libc::c_char) -> libc::c_int;
}

fn main() {
    // C internal modes run before any Rust background thread is created.
    let args: Vec<CString> = std::env::args_os()
        .map(|arg| CString::new(arg.as_bytes()).expect("NUL in process argument"))
        .collect();
    let mut pointers: Vec<_> = args.iter().map(|arg| arg.as_ptr().cast_mut()).collect();
    pointers.push(std::ptr::null_mut());
    // C borrows argv for the duration of this call and never frees it.
    let result = unsafe { hamn_core_main(args.len() as libc::c_int, pointers.as_mut_ptr()) };
    std::process::exit(result);
}
