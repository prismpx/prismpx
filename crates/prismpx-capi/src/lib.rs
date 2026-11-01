//! C ABI for prismpx. See `include/prismpx.h` for the contract.
//!
//! Rules every function follows:
//! - Objects are opaque handles; whoever creates one frees it with the matching `_free`.
//! - Functions that can fail return a status code (`PRISMPX_OK` = 0, negative = error)
//!   and set a thread-local message readable with `prismpx_last_error`.
//! - Panics never cross the boundary; they become `PRISMPX_ERR_PANIC`.

use std::cell::RefCell;
use std::ffi::{c_char, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use prismpx::{Effort, Error, MetadataPolicy, Mode, Options};

pub const PRISMPX_ABI_VERSION: u32 = 2;

pub const PRISMPX_OK: i32 = 0;
pub const PRISMPX_ERR_NULL_POINTER: i32 = -1;
pub const PRISMPX_ERR_INVALID_ARGUMENT: i32 = -2;
pub const PRISMPX_ERR_UNSUPPORTED_FORMAT: i32 = -3;
pub const PRISMPX_ERR_UNSUPPORTED: i32 = -4;
pub const PRISMPX_ERR_MALFORMED: i32 = -5;
pub const PRISMPX_ERR_VERIFICATION_FAILED: i32 = -6;
pub const PRISMPX_ERR_PANIC: i32 = -99;

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_error(msg: impl Into<String>) {
    let msg = CString::new(msg.into().replace('\0', " ")).unwrap_or_default();
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(msg));
}

fn fail(code: i32, msg: impl Into<String>) -> i32 {
    set_error(msg);
    code
}

fn guard(f: impl FnOnce() -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(_) => fail(PRISMPX_ERR_PANIC, "internal panic (this is a bug)"),
    }
}

/// Options handle.
pub struct PrismpxOptions(Options);

/// Result handle.
pub struct PrismpxResult {
    data: Vec<u8>,
    original_size: usize,
    optimized: bool,
    strategy: CString,
}

#[no_mangle]
pub extern "C" fn prismpx_abi_version() -> u32 {
    PRISMPX_ABI_VERSION
}

/// Library version as a static NUL-terminated string.
#[no_mangle]
pub extern "C" fn prismpx_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// Message for the last error on this thread, or NULL. Valid until the next
/// failing call on the same thread.
#[no_mangle]
pub extern "C" fn prismpx_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ref().map_or(ptr::null(), |s| s.as_ptr()))
}

#[no_mangle]
pub extern "C" fn prismpx_options_new() -> *mut PrismpxOptions {
    Box::into_raw(Box::new(PrismpxOptions(Options::default())))
}

/// # Safety
/// `opts` must be NULL or a pointer from `prismpx_options_new` not yet freed.
#[no_mangle]
pub unsafe extern "C" fn prismpx_options_free(opts: *mut PrismpxOptions) {
    if !opts.is_null() {
        drop(Box::from_raw(opts));
    }
}

unsafe fn with_options(opts: *mut PrismpxOptions, f: impl FnOnce(&mut Options) -> i32) -> i32 {
    match opts.as_mut() {
        Some(o) => f(&mut o.0),
        None => fail(PRISMPX_ERR_NULL_POINTER, "options is NULL"),
    }
}

/// `effort`: 0 = fast, 1 = default, 2 = max.
///
/// # Safety
/// `opts` must be a valid options handle.
#[no_mangle]
pub unsafe extern "C" fn prismpx_options_set_effort(opts: *mut PrismpxOptions, effort: i32) -> i32 {
    with_options(opts, |o| {
        o.effort = match effort {
            0 => Effort::Fast,
            1 => Effort::Default,
            2 => Effort::Max,
            _ => return fail(PRISMPX_ERR_INVALID_ARGUMENT, "effort must be 0, 1 or 2"),
        };
        PRISMPX_OK
    })
}

/// `mode`: 0 = lossless (default), 1 = visually lossless. Since ABI 2.
///
/// # Safety
/// `opts` must be a valid options handle.
#[no_mangle]
pub unsafe extern "C" fn prismpx_options_set_mode(opts: *mut PrismpxOptions, mode: i32) -> i32 {
    with_options(opts, |o| {
        o.mode = match mode {
            0 => Mode::Lossless,
            1 => Mode::VisuallyLossless,
            _ => return fail(PRISMPX_ERR_INVALID_ARGUMENT, "mode must be 0 or 1"),
        };
        PRISMPX_OK
    })
}

/// Minimum SSIMULACRA2 score (0..=100) for visually-lossless mode. Since ABI 2.
///
/// # Safety
/// `opts` must be a valid options handle.
#[no_mangle]
pub unsafe extern "C" fn prismpx_options_set_quality(
    opts: *mut PrismpxOptions,
    quality: f32,
) -> i32 {
    with_options(opts, |o| {
        if !(0.0..=100.0).contains(&quality) {
            return fail(
                PRISMPX_ERR_INVALID_ARGUMENT,
                "quality must be within 0..=100",
            );
        }
        o.quality = quality;
        PRISMPX_OK
    })
}

/// `policy`: 0 = safe (default), 1 = keep all, 2 = color only.
///
/// # Safety
/// `opts` must be a valid options handle.
#[no_mangle]
pub unsafe extern "C" fn prismpx_options_set_metadata(
    opts: *mut PrismpxOptions,
    policy: i32,
) -> i32 {
    with_options(opts, |o| {
        o.metadata = match policy {
            0 => MetadataPolicy::Safe,
            1 => MetadataPolicy::KeepAll,
            2 => MetadataPolicy::ColorOnly,
            _ => {
                return fail(
                    PRISMPX_ERR_INVALID_ARGUMENT,
                    "metadata policy must be 0, 1 or 2",
                )
            }
        };
        PRISMPX_OK
    })
}

/// # Safety
/// `opts` must be a valid options handle.
#[no_mangle]
pub unsafe extern "C" fn prismpx_options_set_verify(opts: *mut PrismpxOptions, verify: i32) -> i32 {
    with_options(opts, |o| {
        o.verify = verify != 0;
        PRISMPX_OK
    })
}

/// # Safety
/// `opts` must be a valid options handle.
#[no_mangle]
pub unsafe extern "C" fn prismpx_options_set_clean_transparent(
    opts: *mut PrismpxOptions,
    clean: i32,
) -> i32 {
    with_options(opts, |o| {
        o.clean_transparent = clean != 0;
        PRISMPX_OK
    })
}

/// Optimizes `input_len` bytes at `input`. On success stores a new result
/// handle in `*out` (free it with `prismpx_result_free`).
///
/// # Safety
/// `input` must point to `input_len` readable bytes; `opts` must be NULL
/// (defaults) or a valid options handle; `out` must be a valid pointer.
#[no_mangle]
pub unsafe extern "C" fn prismpx_optimize(
    input: *const u8,
    input_len: usize,
    opts: *const PrismpxOptions,
    out: *mut *mut PrismpxResult,
) -> i32 {
    guard(|| {
        if input.is_null() || out.is_null() {
            return fail(PRISMPX_ERR_NULL_POINTER, "input or out is NULL");
        }
        *out = ptr::null_mut();
        let input = std::slice::from_raw_parts(input, input_len);
        let default = Options::default();
        let opts = opts.as_ref().map_or(&default, |o| &o.0);
        match prismpx::optimize(input, opts) {
            Ok(r) => {
                *out = Box::into_raw(Box::new(PrismpxResult {
                    original_size: r.original_size,
                    optimized: r.optimized,
                    strategy: CString::new(r.strategy).unwrap_or_default(),
                    data: r.data,
                }));
                PRISMPX_OK
            }
            Err(e) => {
                let code = match e {
                    Error::UnsupportedFormat => PRISMPX_ERR_UNSUPPORTED_FORMAT,
                    Error::Unsupported(_) => PRISMPX_ERR_UNSUPPORTED,
                    Error::Malformed(_) => PRISMPX_ERR_MALFORMED,
                    Error::VerificationFailed(_) => PRISMPX_ERR_VERIFICATION_FAILED,
                    _ => PRISMPX_ERR_UNSUPPORTED,
                };
                fail(code, e.to_string())
            }
        }
    })
}

/// Output bytes; length is stored in `*len`. Valid until the result is freed.
///
/// # Safety
/// `res` must be a valid result handle; `len` must be a valid pointer.
#[no_mangle]
pub unsafe extern "C" fn prismpx_result_data(
    res: *const PrismpxResult,
    len: *mut usize,
) -> *const u8 {
    match (res.as_ref(), len.as_mut()) {
        (Some(r), Some(l)) => {
            *l = r.data.len();
            r.data.as_ptr()
        }
        _ => ptr::null(),
    }
}

/// # Safety
/// `res` must be a valid result handle.
#[no_mangle]
pub unsafe extern "C" fn prismpx_result_original_size(res: *const PrismpxResult) -> usize {
    res.as_ref().map_or(0, |r| r.original_size)
}

/// 1 if a smaller encoding was produced, 0 if the original was kept.
///
/// # Safety
/// `res` must be a valid result handle.
#[no_mangle]
pub unsafe extern "C" fn prismpx_result_optimized(res: *const PrismpxResult) -> i32 {
    res.as_ref().map_or(0, |r| r.optimized as i32)
}

/// Description of the chosen strategy. Valid until the result is freed.
///
/// # Safety
/// `res` must be a valid result handle.
#[no_mangle]
pub unsafe extern "C" fn prismpx_result_strategy(res: *const PrismpxResult) -> *const c_char {
    res.as_ref().map_or(ptr::null(), |r| r.strategy.as_ptr())
}

/// # Safety
/// `res` must be NULL or a result handle not yet freed.
#[no_mangle]
pub unsafe extern "C" fn prismpx_result_free(res: *mut PrismpxResult) {
    if !res.is_null() {
        drop(Box::from_raw(res));
    }
}
