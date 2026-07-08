//! C FFI functions for the structured logging library.
//!
//! These `extern "C"` functions are the C binary's interface to structured
//! logging. The C binary calls these after every PTY read, on resize, and
//! on shutdown.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::Path;

use crate::handle::StructuredLogHandle;
use crate::restore;

/// Initialize structured logging for a session.
///
/// Returns an opaque handle, or null on error.
///
/// # Safety
///
/// `session_name` and `log_dir` must be valid null-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn structured_log_init(
    session_name: *const c_char,
    log_dir: *const c_char,
    cols: u32,
    rows: u32,
) -> *mut StructuredLogHandle {
    let name = match unsafe { CStr::from_ptr(session_name) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };
    let dir = match unsafe { CStr::from_ptr(log_dir) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };

    let handle = StructuredLogHandle::new(name, Path::new(dir), cols as usize, rows as usize, None);
    Box::into_raw(Box::new(handle))
}

/// Feed raw PTY output bytes to the structured logger.
///
/// Call this after every PTY read in the C binary.
///
/// # Safety
///
/// `handle` must be a valid pointer from `structured_log_init`.
/// `data` must point to `len` valid bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn structured_log_process(
    handle: *mut StructuredLogHandle,
    data: *const u8,
    len: usize,
) {
    if handle.is_null() || data.is_null() {
        return;
    }
    let handle = unsafe { &mut *handle };
    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
    handle.process(bytes);
}

/// Notify the structured logger of a terminal resize.
///
/// # Safety
///
/// `handle` must be a valid pointer from `structured_log_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn structured_log_resize(
    handle: *mut StructuredLogHandle,
    cols: u32,
    rows: u32,
) {
    if handle.is_null() {
        return;
    }
    let handle = unsafe { &mut *handle };
    handle.resize(cols as usize, rows as usize);
}

/// Flush all writers and free the handle.
///
/// # Safety
///
/// `handle` must be a valid pointer from `structured_log_init`.
/// After this call, the handle is invalid and must not be used.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn structured_log_shutdown(handle: *mut StructuredLogHandle) {
    if handle.is_null() {
        return;
    }
    let mut handle = unsafe { Box::from_raw(handle) };
    handle.shutdown();
    // Box is dropped here, freeing the handle
}

/// Restore a session from its `.grid.jsonl` file.
///
/// Reads the grid log, produces ANSI output, and writes it to stdout.
/// Returns 0 on success, -1 on error (caller should fall back to legacy restore).
///
/// # Safety
///
/// `log_dir` and `session_name` must be valid null-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn structured_log_restore(
    log_dir: *const c_char,
    session_name: *const c_char,
) -> i32 {
    let dir = match unsafe { CStr::from_ptr(log_dir) }.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let name = match unsafe { CStr::from_ptr(session_name) }.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };

    match restore::restore_session(Path::new(dir), name) {
        Some(restore) => {
            print!("{}", restore.ansi);
            0
        }
        None => -1,
    }
}
