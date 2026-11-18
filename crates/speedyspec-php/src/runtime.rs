//! Boots the embedded PHP engine and runs code in it.
//!
//! The engine runs under a small custom SAPI built with ext-php-rs's
//! `SapiBuilder` rather than PHP's stock embed SAPI, because only
//! `php_module_startup` lets us hand PHP our own module: that module is how
//! functions (and, later, classes) implemented in Rust become visible to PHP.

use std::ffi::{CString, c_char, c_int, c_void};
use std::fmt;
use std::io::Write;
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use ext_php_rs::builders::SapiBuilder;
use ext_php_rs::embed::{
    Embed, EmbedError, SapiModule, cleanup_sapi_allocations, ext_php_rs_sapi_shutdown,
    ext_php_rs_sapi_startup,
};
use ext_php_rs::ffi::{
    ZEND_RESULT_CODE_SUCCESS, php_module_shutdown, php_module_startup, php_request_shutdown,
    php_request_startup, sapi_shutdown, sapi_startup, zend_eval_string,
};
use ext_php_rs::prelude::*;
use ext_php_rs::zend::{ExecutorGlobals, try_catch_first};

/// INI settings the CLI needs regardless of any php.ini, which is never read.
/// These follow what PHP's own CLI SAPI forces, except `register_argc_argv`:
/// outside the CLI SAPI PHP 8.5 would derive `$argv` from the query string
/// (deprecated), so `run_file` sets `$argv` and `$argc` itself.
const INI_ENTRIES: &std::ffi::CStr = c"html_errors=0
display_errors=1
log_errors=0
register_argc_argv=0
implicit_flush=1
output_buffering=0
max_execution_time=0
max_input_time=-1
";

static BOOTED: AtomicBool = AtomicBool::new(false);

/// Why PHP could not boot or run something.
#[derive(Debug)]
#[non_exhaustive]
pub enum PhpError {
    /// `Php::boot` was called a second time in this process.
    AlreadyBooted,
    /// The engine refused to start; the string names the step.
    Startup(&'static str),
    /// A label, code string or path contained a NUL byte.
    NulByte,
    /// The script passed to `run_file` does not exist.
    ScriptNotFound(PathBuf),
    /// PHP code threw an exception that nothing caught.
    UncaughtException { label: String, class: String },
    /// PHP code failed to compile or execute.
    Failed { label: String },
    /// PHP hit a fatal error and abandoned the code it was running.
    Fatal { label: String },
}

impl fmt::Display for PhpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyBooted => f.write_str("PHP is already booted in this process"),
            Self::Startup(step) => write!(f, "PHP failed to start: {step}"),
            Self::NulByte => f.write_str("PHP input contains a NUL byte"),
            Self::ScriptNotFound(path) => write!(f, "PHP script not found: {}", path.display()),
            Self::UncaughtException { label, class } => {
                write!(f, "{label}: uncaught {class}")
            }
            Self::Failed { label } => write!(f, "{label}: PHP code failed"),
            Self::Fatal { label } => write!(f, "{label}: PHP fatal error"),
        }
    }
}

impl std::error::Error for PhpError {}

/// A booted PHP engine with one request open.
///
/// There is at most one per process, and it is neither `Send` nor `Sync`:
/// the build is non-thread-safe, so PHP must only be touched from the thread
/// that booted it. Dropping it shuts PHP down.
pub struct Php {
    sapi: *mut SapiModule,
    _not_send: PhantomData<*mut ()>,
}

impl Php {
    /// Starts PHP and opens the request that all later calls run in.
    ///
    /// php.ini and the INI scan directory are never read. Native functions
    /// implemented in Rust are registered through [`module`].
    pub fn boot() -> Result<Self, PhpError> {
        if BOOTED.swap(true, Ordering::SeqCst) {
            return Err(PhpError::AlreadyBooted);
        }

        let sapi = SapiBuilder::new("speedyspec", "SpeedySpec")
            .ub_write_function(ub_write)
            .flush_function(flush)
            .log_message_function(log_message)
            .php_ini_ignore(1)
            .build()
            .map_err(|_| PhpError::Startup("building the SAPI"))?;
        // PHP copies the struct, but cleanup_sapi_allocations needs the
        // original after shutdown, so it lives on the heap until Drop. If
        // startup fails it is leaked: a half-started engine cannot be shut
        // down safely, and the process is about to report the error and exit.
        let sapi = Box::into_raw(Box::new(sapi));

        // SAFETY: the documented SAPI start sequence, run once per process
        // (guarded by BOOTED) on this thread. `sapi_startup` clears
        // `ini_entries`, so it is set between the two startup calls, as
        // php-src's own SAPIs do. INI_ENTRIES is 'static.
        unsafe {
            ext_php_rs_sapi_startup();
            sapi_startup(sapi);
            (*sapi).ini_entries = INI_ENTRIES.as_ptr();
            if php_module_startup(sapi, get_module()) != ZEND_RESULT_CODE_SUCCESS {
                return Err(PhpError::Startup("php_module_startup"));
            }
            if php_request_startup() != ZEND_RESULT_CODE_SUCCESS {
                return Err(PhpError::Startup("php_request_startup"));
            }
        }

        Ok(Self {
            sapi,
            _not_send: PhantomData,
        })
    }

    /// Runs PHP statements (no `<?php` tag). `label` names the code in PHP's
    /// error messages, where a file name would otherwise go.
    pub fn eval(&self, label: &str, code: &str) -> Result<(), PhpError> {
        let c_label = CString::new(label).map_err(|_| PhpError::NulByte)?;
        let c_code = CString::new(code).map_err(|_| PhpError::NulByte)?;

        // SAFETY: called on the booting thread inside the open request. A
        // null retval makes PHP compile `code` as statements, not an
        // expression.
        let outcome = try_catch_first(AssertUnwindSafe(|| unsafe {
            zend_eval_string(c_code.as_ptr(), std::ptr::null_mut(), c_label.as_ptr())
        }));

        if let Some(exception) = ExecutorGlobals::take_exception() {
            let class = exception
                .get_class_name()
                .unwrap_or_else(|_| "exception".to_owned());
            return Err(PhpError::UncaughtException {
                label: label.to_owned(),
                class,
            });
        }

        match outcome {
            Ok(ZEND_RESULT_CODE_SUCCESS) => Ok(()),
            Ok(_) => Err(PhpError::Failed {
                label: label.to_owned(),
            }),
            Err(_) => Err(PhpError::Fatal {
                label: label.to_owned(),
            }),
        }
    }

    /// Runs a PHP file the way `php <path> <argv...>` would and returns its
    /// exit status. `$argv` is `[path, ...argv]` and `$argc` its length.
    /// An uncaught exception or fatal error is reported by PHP itself and
    /// yields a non-zero status, not an `Err`.
    pub fn run_file(&self, path: &Path, argv: &[String]) -> Result<i32, PhpError> {
        if !path.is_file() {
            return Err(PhpError::ScriptNotFound(path.to_owned()));
        }

        let mut args = Vec::with_capacity(argv.len() + 1);
        args.push(path.to_string_lossy().into_owned());
        args.extend(argv.iter().cloned());

        let label = path.display().to_string();
        {
            let mut globals = ExecutorGlobals::get_mut();
            let argc = i64::try_from(args.len()).unwrap_or(i64::MAX);
            let symbols = &mut globals.symbol_table;
            symbols
                .insert("argv", args)
                .and_then(|()| symbols.insert("argc", argc))
                .map_err(|_| PhpError::Failed {
                    label: label.clone(),
                })?;
            globals.exit_status = 0;
        }

        let outcome = Embed::run_script(path);
        let status = ExecutorGlobals::get().exit_status;

        match outcome {
            Ok(()) => Ok(status),
            Err(EmbedError::InvalidEvalString(_)) => Err(PhpError::NulByte),
            Err(EmbedError::InvalidPath) => Err(PhpError::ScriptNotFound(path.to_owned())),
            // PHP has already printed the error; make sure it reads as failure.
            Err(EmbedError::ExecuteScriptError | EmbedError::CatchError(_)) => {
                Ok(if status == 0 { 255 } else { status })
            }
            Err(_) => Err(PhpError::Failed { label }),
        }
    }
}

impl Drop for Php {
    fn drop(&mut self) {
        // SAFETY: reverse of the start sequence in `boot`, on the same thread.
        // The SAPI module is reclaimed only after PHP no longer uses it.
        unsafe {
            php_request_shutdown(std::ptr::null_mut());
            php_module_shutdown();
            sapi_shutdown();
            ext_php_rs_sapi_shutdown();
            cleanup_sapi_allocations(self.sapi);
            drop(Box::from_raw(self.sapi));
        }
    }
}

/// Adds two integers. Exists to prove PHP can call a function implemented
/// in Rust; later tasks replace it with the real native API.
#[php_function]
pub fn speedyspec_proof_add(a: i64, b: i64) -> PhpResult<i64> {
    a.checked_add(b)
        .ok_or_else(|| "speedyspec_proof_add(): integer overflow".into())
}

/// The PHP module whose functions (and classes) are compiled into this
/// binary. `php_module_startup` registers it like a built-in extension.
#[php_module]
pub fn module(module: ModuleBuilder) -> ModuleBuilder {
    module.function(wrap_function!(speedyspec_proof_add))
}

/// Sends PHP's output to the process's stdout.
extern "C" fn ub_write(data: *const c_char, len: usize) -> usize {
    // SAFETY: PHP passes a buffer of `len` bytes that outlives the call.
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) };
    match std::io::stdout().lock().write_all(bytes) {
        Ok(()) => len,
        Err(_) => 0,
    }
}

extern "C" fn flush(_server_context: *mut c_void) {
    let _ = std::io::stdout().flush();
}

/// Sends PHP's log messages (startup errors, `error_log()`) to stderr.
extern "C" fn log_message(message: *const c_char, _syslog_type: c_int) {
    // SAFETY: PHP passes a NUL-terminated string that outlives the call.
    let message = unsafe { std::ffi::CStr::from_ptr(message) };
    eprintln!("{}", message.to_string_lossy());
}
