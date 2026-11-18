//! Boots the embedded PHP engine, runs code in it, and keeps PHP's error
//! handling on PHP's side of the boundary.
//!
//! The engine runs under a small custom SAPI built with ext-php-rs's
//! `SapiBuilder` rather than PHP's stock embed SAPI, because only
//! `php_module_startup` lets us hand PHP our own module: that module is how
//! functions and classes implemented in Rust become visible to PHP.
//!
//! # Bailouts, exceptions and `exit()`
//!
//! A PHP fatal error is a bailout: a `longjmp` to the innermost `zend_try`.
//! It must never cross a Rust frame that owns something with a destructor.
//! The rule this module enforces (see also `docs/php-embed.md`):
//!
//! 1. Every entry into PHP goes through [`guard`]: a `zend_try` around a
//!    closure that captures only references and raw pointers, so the only
//!    frames a bailout can skip own nothing. Whatever the closure works on
//!    is owned by the caller, outside, and is dropped normally afterwards.
//! 2. Native functions called by PHP go through [`native_entry`]. If a guard
//!    inside one catches a bailout, it is recorded instead of continuing; the
//!    native function returns normally through its Rust frames, and only
//!    then, in a frame that owns nothing, [`native_entry`] re-raises it to
//!    the PHP code below. While a re-raise is pending no PHP code runs.
//! 3. A Rust panic in a native function cannot unwind through PHP's C
//!    frames. It is caught there, parked, and carried through PHP as a
//!    bailout; the next guard below resumes it in Rust. A panic is never
//!    reported as a PHP fatal error.
//! 4. Releasing a PHP value can run PHP code (a destructor, the cycle
//!    collector), so Rust never releases one from `Drop`: dropping a
//!    [`ZvalHandle`](crate::convert::ZvalHandle) queues the reference with
//!    [`release_later`], and [`drain`] releases the queue, each value under
//!    its own guard, at the end of every entry from or into PHP.
//!
//! An exception is not a bailout: it is left in `EG(exception)` and taken
//! out by whoever called PHP. `exit()` throws PHP's internal unwind-exit
//! object; when it happens below a native function it is carried back up to
//! that function, which rethrows it, so the PHP code that called in unwinds
//! as PHP's own `exit()` would.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt;
use std::io::Write;
use std::marker::PhantomData;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use ext_php_rs::builders::SapiBuilder;
use ext_php_rs::embed::{
    SapiModule, cleanup_sapi_allocations, ext_php_rs_sapi_shutdown, ext_php_rs_sapi_startup,
};
use ext_php_rs::ffi::{
    _call_user_function_impl, _efree, _zend_compile_position_ZEND_COMPILE_POSITION_AFTER_OPEN_TAG,
    _zend_compile_position_ZEND_COMPILE_POSITION_AT_OPEN_TAG, E_COMPILE_ERROR, E_CORE_ERROR,
    E_ERROR, E_PARSE, E_RECOVERABLE_ERROR, E_USER_ERROR, ZEND_RESULT_CODE_SUCCESS,
    destroy_op_array, ext_php_rs_compiler_globals, ext_php_rs_executor_globals,
    ext_php_rs_process_globals, ext_php_rs_zend_string_init, ext_php_rs_zend_string_release,
    php_module_shutdown, php_module_startup, php_request_shutdown, php_request_startup,
    sapi_shutdown, sapi_startup, zend_compile_position, zend_compile_string,
    zend_destroy_static_vars, zend_execute, zend_execute_data, zend_function,
    zend_get_executed_scope, zend_hash_str_find, zend_object, zend_op_array, zend_read_property,
    zend_string, zval, zval_ptr_dtor,
};
use ext_php_rs::types::Zval;
use ext_php_rs::zend::{ExecuteData, bailout, try_catch};
use speedyspec_hooks::{SharedHooks, Value};

use crate::convert::{self, ConvertError, Unconvertible, ZvalHandle};
use crate::hook_class;

// Engine functions that ext-php-rs's bindings leave out. They are ZEND_API
// symbols of the statically linked libphp.
unsafe extern "C" {
    fn zend_is_unwind_exit(ex: *const zend_object) -> bool;
    fn zend_is_graceful_exit(ex: *const zend_object) -> bool;
    fn zend_throw_exception_internal(exception: *mut zend_object);
}

/// INI settings the CLI needs regardless of any php.ini, which is never read.
/// These follow what PHP's own CLI SAPI forces, except `register_argc_argv`:
/// outside the CLI SAPI PHP 8.5 would derive `$argv` from the query string
/// (deprecated), so `run_file` sets `$argv` and `$argc` itself.
const INI_ENTRIES: &CStr = c"html_errors=0
display_errors=1
log_errors=0
register_argc_argv=0
implicit_flush=1
output_buffering=0
max_execution_time=0
max_input_time=-1
";

/// `SpeedySpec\Hooks\HookException`, evaluated at boot before the preload.
const HOOK_EXCEPTION_PHP: &str = r"namespace SpeedySpec\Hooks;

/**
 * Reports a hook failure that is not a callback's own exception, such as a broken backend.
 */
class HookException extends \RuntimeException
{
}
";

/// Stands in for the file name of [`HOOK_EXCEPTION_PHP`] in PHP's messages.
const HOOK_EXCEPTION_LABEL: &str = "speedyspec://hook-exception";

/// An internal function whose `zend_function` gives a dummy frame something
/// to point at; any internal function would do.
const DUMMY_FRAME_FUNCTION: &str = "apply_filters";

static BOOTED: AtomicBool = AtomicBool::new(false);
/// Where PHP's output goes: false = stdout, true = stderr. Read by the SAPI's
/// output callback, which has no other way to reach the `Php`.
static OUTPUT_TO_STDERR: AtomicBool = AtomicBool::new(false);

/// Why PHP could not boot or run something.
#[derive(Debug)]
#[non_exhaustive]
pub enum PhpError {
    /// The engine could not start (or was already started in this process).
    Boot(String),
    /// PHP code threw something that nothing caught.
    Exception {
        class: String,
        message: String,
        file: String,
        line: u32,
    },
    /// PHP hit a fatal error and abandoned the code it was running.
    Fatal {
        message: String,
        file: String,
        line: u32,
    },
    /// The code called `exit()` or `die()` with this status.
    Exit(i32),
    /// A label, code string or path contained a NUL byte.
    NulByte,
    /// The script passed to `run_file` does not exist.
    ScriptNotFound(PathBuf),
    /// The path is not valid UTF-8, so it cannot be handed to PHP as `$argv`.
    NonUtf8Path(PathBuf),
    /// A value could not be converted between PHP and Rust.
    Conversion(String),
}

impl fmt::Display for PhpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Boot(why) => write!(f, "PHP failed to start: {why}"),
            Self::Exception {
                class,
                message,
                file,
                line,
            } => write!(f, "{file}:{line}: uncaught {class}: {message}"),
            Self::Fatal {
                message,
                file,
                line,
            } => write!(f, "{file}:{line}: PHP fatal error: {message}"),
            Self::Exit(status) => write!(f, "PHP code called exit({status})"),
            Self::NulByte => f.write_str("PHP input contains a NUL byte"),
            Self::ScriptNotFound(path) => write!(f, "PHP script not found: {}", path.display()),
            Self::NonUtf8Path(path) => {
                write!(f, "PHP script path is not valid UTF-8: {}", path.display())
            }
            Self::Conversion(why) => write!(f, "PHP value conversion failed: {why}"),
        }
    }
}

impl std::error::Error for PhpError {}

/// Where PHP's output (`echo`, error display, ...) is written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Output {
    #[default]
    Stdout,
    Stderr,
}

/// What [`Php::boot`] needs.
pub struct BootOptions<'a> {
    /// The hook registry PHP's `SpeedySpec\Hooks\Hook` uses.
    pub hooks: SharedHooks,
    /// `(file name, PHP source)` pairs evaluated at boot, in order, before
    /// any other PHP code. The source is a file's content, `<?php` tag and
    /// all; the name stands in for the file name in PHP's messages.
    pub preload: &'a [(&'a str, &'a str)],
}

/// A booted PHP engine with one request open.
///
/// There is at most one per process, and it is neither `Send` nor `Sync`:
/// the build is non-thread-safe, so PHP must only be touched from the thread
/// that booted it. Dropping it shuts PHP down.
///
/// # PHP values that outlive the engine
///
/// Dropping `Php` first releases every PHP value Rust has already let go of,
/// then ends the request, which destroys every PHP object, including those
/// Rust still holds (PHP callbacks in the registry, `Value::Opaque`s kept by
/// Rust code). Those handles are inert from then on: dropping one releases
/// nothing (the engine freed the memory), and firing a hook whose callback is
/// a PHP callable returns a `CallbackError` without touching PHP.
pub struct Php {
    sapi: *mut SapiModule,
    _not_send: PhantomData<*mut ()>,
}

impl Php {
    /// Starts PHP, opens the request that all later calls run in, defines
    /// `SpeedySpec\Hooks\HookException` and evaluates `options.preload`.
    ///
    /// php.ini and the INI scan directory are never read. Like the PHP CLI,
    /// the engine skips a leading `#!` line in every file it compiles.
    pub fn boot(options: BootOptions<'_>) -> Result<Self, PhpError> {
        if BOOTED.swap(true, Ordering::SeqCst) {
            return Err(PhpError::Boot(
                "PHP is already booted in this process".to_owned(),
            ));
        }

        let sapi = SapiBuilder::new("speedyspec", "SpeedySpec")
            .ub_write_function(ub_write)
            .flush_function(flush)
            .log_message_function(log_message)
            .php_ini_ignore(1)
            .build()
            .map_err(|e| PhpError::Boot(format!("building the SAPI: {e}")))?;
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
            if php_module_startup(sapi, hook_class::module_entry()) != ZEND_RESULT_CODE_SUCCESS {
                return Err(PhpError::Boot("php_module_startup failed".to_owned()));
            }
            if php_request_startup() != ZEND_RESULT_CODE_SUCCESS {
                return Err(PhpError::Boot("php_request_startup failed".to_owned()));
            }
            // As the CLI does; request startup has just reset it.
            (*ext_php_rs_compiler_globals()).skip_shebang = true;
        }

        // From here on, failing drops `php`, which shuts the engine down.
        let php = Self {
            sapi,
            _not_send: PhantomData,
        };

        // SAFETY: the request is open, so the function table is populated
        // and stays so until shutdown.
        let dummy = unsafe { find_function(DUMMY_FRAME_FUNCTION) };
        if dummy.is_null() {
            return Err(PhpError::Boot(format!(
                "the native function {DUMMY_FRAME_FUNCTION}() is not registered"
            )));
        }
        ENGINE.with(|e| {
            e.dummy_func.set(dummy);
            e.alive.set(true);
        });
        hook_class::install(options.hooks);
        php.execute(
            HOOK_EXCEPTION_LABEL,
            HOOK_EXCEPTION_PHP.as_bytes(),
            _zend_compile_position_ZEND_COMPILE_POSITION_AFTER_OPEN_TAG,
        )?;
        hook_class::find_exception_class()?;
        for (name, source) in options.preload {
            php.execute(
                name,
                source.as_bytes(),
                _zend_compile_position_ZEND_COMPILE_POSITION_AT_OPEN_TAG,
            )?;
        }
        Ok(php)
    }

    /// Runs PHP statements (no `<?php` tag) and returns what a top-level
    /// `return` gives back, or null. `label` names the code in PHP's error
    /// messages, where a file name would otherwise go.
    pub fn eval(&self, label: &str, code: &str) -> Result<Value, PhpError> {
        self.execute(
            label,
            code.as_bytes(),
            _zend_compile_position_ZEND_COMPILE_POSITION_AFTER_OPEN_TAG,
        )
    }

    /// `require`s a PHP file and returns its return value (1 if it returns
    /// nothing). A relative path is resolved against the working directory,
    /// never against `include_path`. Exceptions, fatal errors and `exit()`
    /// come back as errors.
    pub fn require(&self, path: &Path) -> Result<Value, PhpError> {
        let label = path
            .to_str()
            .ok_or_else(|| PhpError::NonUtf8Path(path.to_owned()))?;
        if label.contains('\0') {
            return Err(PhpError::NulByte);
        }
        let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        let absolute = absolute
            .to_str()
            .ok_or_else(|| PhpError::NonUtf8Path(path.to_owned()))?;
        let code = format!("return require {};", php_quote(absolute));
        self.eval(label, &code)
    }

    /// Runs a PHP file the way `php <path> <argv...>` would and returns its
    /// exit status: 0 when it ends normally, `n` when it calls `exit(n)`.
    /// `$argv` (and `$_SERVER['argv']`) is `[path, ...argv]` and `$argc` its
    /// length. An uncaught exception or fatal error is an `Err`.
    pub fn run_file(&self, path: &Path, argv: &[String]) -> Result<i32, PhpError> {
        let script = path
            .to_str()
            .ok_or_else(|| PhpError::NonUtf8Path(path.to_owned()))?;
        if !path.is_file() {
            return Err(PhpError::ScriptNotFound(path.to_owned()));
        }

        let mut code = String::from("$GLOBALS['argv'] = [");
        for arg in std::iter::once(script).chain(argv.iter().map(String::as_str)) {
            code.push_str(&php_quote(arg));
            code.push(',');
        }
        code.push_str(
            "]; $GLOBALS['argc'] = \\count($GLOBALS['argv']); \
             $_SERVER['argv'] = $GLOBALS['argv']; $_SERVER['argc'] = $GLOBALS['argc'];",
        );
        self.eval("speedyspec://argv", &code)?;

        // SAFETY: plain write to the executor globals on the engine's thread.
        unsafe { (*ext_php_rs_executor_globals()).exit_status = 0 };
        match self.require(path) {
            Ok(_) => Ok(0),
            Err(PhpError::Exit(status)) => Ok(status),
            Err(err) => Err(err),
        }
    }

    /// Calls a PHP callable (a function name, a `[class, method]` array, a
    /// closure or an invokable object) with `args` and returns its result.
    pub fn call(&self, callable: &Value, args: &[Value]) -> Result<Value, PhpError> {
        let mut target = ManuallyDrop::new(Zval::new());
        let target_ptr: *mut zval = &raw mut *target;
        let written = guard(|| {
            // SAFETY: `target` is fresh zval storage owned by this frame.
            unsafe { convert::write_value(target_ptr, callable) }
        });
        let outcome = match written {
            Ok(Ok(())) => call_php(target_ptr, args),
            Ok(Err(why)) => Err(CallError::Convert(why.to_string())),
            Err(interrupted) => Err(CallError::Interrupted(interrupted)),
        };
        release_later(target);
        drain();
        outcome.map_err(CallError::into_php_error)
    }

    /// The class of a PHP object (`Value::Opaque` from PHP); `None` for
    /// anything else.
    pub fn class_of(&self, value: &Value) -> Option<String> {
        let obj = convert::handle_of(value)?.object()?;
        // SAFETY: the handle keeps the object, and so its class, alive.
        Some(unsafe { class_name(obj) })
    }

    /// The public properties of a PHP object, in declaration then insertion
    /// order; `None` for anything that is not a PHP object, or if one of the
    /// properties cannot be converted.
    pub fn properties_of(&self, value: &Value) -> Option<Vec<(String, Value)>> {
        let obj = convert::handle_of(value)?.object()?;
        // Fetching the table can run PHP code (a lazy object initialises).
        let table = guard(|| {
            // SAFETY: the handle keeps `obj` alive; every object has handlers
            // with a get_properties entry.
            unsafe { (*(*obj).handlers).get_properties.map(|get| get(obj)) }
        });
        let table = match table {
            Ok(Some(table)) if !table.is_null() => table,
            _ => return None,
        };
        if let Some(thrown) = take_thrown() {
            drop(thrown);
            drain();
            return None;
        }
        let mut props = Vec::new();
        // SAFETY: the object owns `table` and is kept alive by the handle;
        // reading it runs no PHP code.
        let read = unsafe { convert::to_value(&raw const *table_as_zval(table)) };
        let Ok(Value::Array(items)) = read else {
            return None;
        };
        for (key, item) in items {
            match key {
                // Protected and private names are mangled as "\0*\0name" and
                // "\0Class\0name".
                speedyspec_hooks::Key::Str(name) if name.starts_with('\0') => {}
                speedyspec_hooks::Key::Str(name) => props.push((name, item)),
                speedyspec_hooks::Key::Int(i) => props.push((i.to_string(), item)),
            }
        }
        Some(props)
    }

    /// Names the plugin that later `Hook::add` calls are registered for, until
    /// changed. Hook errors from a failing callback name its owner.
    pub fn set_owner(&self, owner: Option<&str>) {
        hook_class::set_owner(owner);
    }

    /// Sends PHP's output to stdout (the default) or stderr.
    pub fn redirect_output(&self, to: Output) {
        flush(ptr::null_mut());
        OUTPUT_TO_STDERR.store(to == Output::Stderr, Ordering::SeqCst);
    }

    /// Compiles `code` as `label` and runs it at the current scope.
    fn execute(
        &self,
        label: &str,
        code: &[u8],
        position: zend_compile_position,
    ) -> Result<Value, PhpError> {
        let label = CString::new(label).map_err(|_| PhpError::NulByte)?;
        let label_ptr = label.as_ptr();

        // Owned here, outside every guard; the guards only see pointers.
        let mut retval = ManuallyDrop::new(Zval::new());
        let retval_ptr: *mut zval = &raw mut *retval;
        let source: Cell<*mut zend_string> = Cell::new(ptr::null_mut());
        let op_array: Cell<*mut zend_op_array> = Cell::new(ptr::null_mut());
        // SAFETY: plain reads of the compiler globals on the engine's thread.
        let saved_options = unsafe { (*ext_php_rs_compiler_globals()).compiler_options };

        let ran = guard(|| {
            // SAFETY: the sequence of zend_eval_stringl, keeping the op array
            // so its top-level `return` reaches `retval`. All pointers are
            // owned by the enclosing frame.
            unsafe {
                source.set(ext_php_rs_zend_string_init(
                    code.as_ptr().cast(),
                    code.len(),
                    false,
                ));
                let cg = ext_php_rs_compiler_globals();
                (*cg).compiler_options = 0; // ZEND_COMPILE_DEFAULT_FOR_EVAL
                let compile = zend_compile_string.expect("zend_compile_string is always set");
                let op = compile(source.get(), label_ptr, position);
                (*cg).compiler_options = saved_options;
                if op.is_null() {
                    return; // A parse error is pending in EG(exception).
                }
                op_array.set(op);
                (*op).scope = zend_get_executed_scope();
                let eg = ext_php_rs_executor_globals();
                (*eg).no_extensions = true;
                zend_execute(op, retval_ptr);
                (*eg).no_extensions = false;
                zend_destroy_static_vars(op);
                destroy_op_array(op);
                _efree(op.cast());
                op_array.set(ptr::null_mut());
            }
        });

        // SAFETY: restoring what the guard set, in case it bailed out early.
        unsafe {
            (*ext_php_rs_compiler_globals()).compiler_options = saved_options;
            (*ext_php_rs_executor_globals()).no_extensions = false;
        }
        let fatal = ran.err().map(fatal_error);
        let op = op_array.get();
        if !op.is_null() {
            // A bailout during execution: free the op array as
            // zend_eval_stringl does. Refused if a re-raise is pending, in
            // which case it stays with the request until shutdown.
            let _ = guard(|| {
                // SAFETY: `op` was compiled above and not yet destroyed.
                unsafe {
                    destroy_op_array(op);
                    _efree(op.cast());
                }
            });
        }
        let src = source.get();
        if !src.is_null() {
            // SAFETY: the reference created above; freeing a string runs no
            // PHP code.
            unsafe { ext_php_rs_zend_string_release(src) };
        }

        // SAFETY: `retval` is either still null or what the code returned.
        let value = unsafe { convert::to_value(retval_ptr) };
        release_later(retval);
        drain();
        if let Some(fatal) = fatal {
            discard_exception();
            return Err(fatal);
        }
        if let Some(thrown) = take_thrown() {
            return Err(thrown.into_php_error());
        }
        value.map_err(|e| PhpError::Conversion(e.to_string()))
    }
}

impl Drop for Php {
    fn drop(&mut self) {
        // Release everything Rust has let go of while PHP code may still run.
        drain();
        // SAFETY: ends the request on the booting thread. Native functions
        // still work during it (shutdown functions and destructors may fire
        // hooks); PHP catches bailouts in each shutdown step itself.
        unsafe { php_request_shutdown(ptr::null_mut()) };

        // The request freed every PHP value. Any reference Rust still holds
        // or has queued points into freed memory and must never be touched.
        let parked_panic = ENGINE.with(|e| {
            e.alive.set(false);
            let queued = std::mem::take(&mut *e.releases.borrow_mut());
            drop(queued); // ManuallyDrop: nothing is released.
            e.pending_exit.set(ptr::null_mut());
            e.unwinding.set(false);
            e.panic.borrow_mut().take()
        });
        hook_class::uninstall();

        // SAFETY: reverse of the start sequence in `boot`, on the same thread.
        // The SAPI module is reclaimed only after PHP no longer uses it.
        unsafe {
            php_module_shutdown();
            sapi_shutdown();
            ext_php_rs_sapi_shutdown();
            cleanup_sapi_allocations(self.sapi);
            drop(Box::from_raw(self.sapi));
        }

        // A Rust panic inside a native function called during shutdown.
        if let Some(payload) = parked_panic
            && !std::thread::panicking()
        {
            resume_unwind(payload);
        }
    }
}

// ---------------------------------------------------------------------------
// Engine state shared by the runtime and the native functions

/// Per-thread engine state. PHP only ever runs on one thread, the one that
/// booted it, and every native function and SAPI callback runs there too.
struct Engine {
    /// Between boot and the end of the request in `Drop`.
    alive: Cell<bool>,
    /// Native functions (called by PHP) currently on the stack.
    native_depth: Cell<u32>,
    /// A guard inside a native function caught a bailout; the innermost
    /// native function must re-raise it. No PHP code runs meanwhile.
    unwinding: Cell<bool>,
    /// `exit()` happened below a native function; that function must rethrow
    /// this unwind-exit object (one owned reference). No PHP code runs
    /// meanwhile.
    pending_exit: Cell<*mut zend_object>,
    /// A panic carried through PHP by a bailout, to resume in Rust.
    panic: RefCell<Option<Box<dyn Any + Send>>>,
    /// References waiting to be released by [`drain`].
    releases: RefCell<Vec<ManuallyDrop<Zval>>>,
    /// How many times [`guard`] has entered PHP.
    entries: Cell<u64>,
    /// Gives a dummy frame a `func`; see [`call_php`].
    dummy_func: Cell<*mut zend_function>,
}

thread_local! {
    static ENGINE: Engine = const {
        Engine {
            alive: Cell::new(false),
            native_depth: Cell::new(0),
            unwinding: Cell::new(false),
            pending_exit: Cell::new(ptr::null_mut()),
            panic: RefCell::new(None),
            releases: RefCell::new(Vec::new()),
            entries: Cell::new(0),
            dummy_func: Cell::new(ptr::null_mut()),
        }
    };
}

/// How many times the bridge has entered the PHP engine since the process
/// started. Every entry goes through [`guard`], which counts it.
pub(crate) fn entries() -> u64 {
    ENGINE.with(|e| e.entries.get())
}

/// True from boot until the request ends in `Php::drop`.
pub(crate) fn alive() -> bool {
    ENGINE.with(|e| e.alive.get())
}

/// False after shutdown, or while a bailout or `exit()` is on its way to the
/// innermost native function (PHP must not be entered until it has been
/// re-raised).
pub(crate) fn can_enter() -> bool {
    alive() && !interrupted()
}

/// True while a bailout or `exit()` is on its way to the innermost native
/// function; PHP must not be entered until it has been re-raised.
fn interrupted() -> bool {
    ENGINE.with(|e| e.unwinding.get() || !e.pending_exit.get().is_null())
}

/// Why a [`guard`]ed call did not return.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Interrupted {
    /// PHP bailed out (a fatal error).
    Bailout,
    /// PHP was not entered: the engine is down, or a bailout or `exit()` is
    /// unwinding to a native function.
    Refused,
}

/// Runs `f` inside a `zend_try`, the only way any code here enters PHP.
///
/// `f` must capture only references and raw pointers: a bailout skips its
/// frame, so nothing it owns would be dropped. A panic inside `f` is resumed
/// as soon as `f` has unwound; a panic carried through PHP from a native
/// function deeper down is resumed here too.
pub(crate) fn guard<R>(f: impl FnOnce() -> R) -> Result<R, Interrupted> {
    if !can_enter() {
        return Err(Interrupted::Refused);
    }
    ENGINE.with(|e| e.entries.set(e.entries.get() + 1));
    // The plain (not "first") zend_try saves and restores the enclosing
    // EG(bailout), so guards nest: PHP code running inside one may call a
    // native function that enters another.
    let outcome = try_catch(AssertUnwindSafe(|| catch_unwind(AssertUnwindSafe(f))));
    match outcome {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(payload)) => resume_unwind(payload),
        Err(_) => {
            if let Some(payload) = ENGINE.with(|e| e.panic.borrow_mut().take()) {
                resume_unwind(payload);
            }
            ENGINE.with(|e| {
                if e.native_depth.get() > 0 {
                    // PHP code below a native function was abandoned
                    // mid-call (EG(current_execute_data) is now null): it
                    // must keep unwinding, see `native_entry`.
                    e.unwinding.set(true);
                }
            });
            Err(Interrupted::Bailout)
        }
    }
}

/// The body of every native function PHP calls. See the module docs.
pub(crate) fn native_entry(
    ex: &mut ExecuteData,
    retval: &mut Zval,
    body: fn(&mut ExecuteData, &mut Zval),
) {
    if run_native(ex, retval, body) {
        // SAFETY: this frame owns nothing; `run_native` has returned, so every
        // Rust value of the call has been dropped. The bailout continues to
        // the zend_try below, as it would have without us.
        unsafe { bailout() }
    }
}

/// Runs `body` and decides how the native call ends; true means re-raise a
/// bailout.
#[inline(never)]
fn run_native(
    ex: &mut ExecuteData,
    retval: &mut Zval,
    body: fn(&mut ExecuteData, &mut Zval),
) -> bool {
    ENGINE.with(|e| e.native_depth.set(e.native_depth.get() + 1));
    // Releasing what the call let go of is still part of the call: a
    // destructor that bails out must be re-raised like any other bailout.
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        body(ex, retval);
        drain();
    }));
    ENGINE.with(|e| e.native_depth.set(e.native_depth.get() - 1));
    if let Err(payload) = outcome {
        // Carry the panic through the PHP frames below as a bailout.
        ENGINE.with(|e| *e.panic.borrow_mut() = Some(payload));
        return true;
    }
    if ENGINE.with(|e| e.unwinding.replace(false)) {
        return true;
    }
    let exit = ENGINE.with(|e| e.pending_exit.replace(ptr::null_mut()));
    if !exit.is_null() {
        // SAFETY: hands our reference to EG(exception). The current frame is
        // this native function's, so PHP unwinds its caller as exit() does.
        unsafe { zend_throw_exception_internal(exit) };
    }
    false
}

/// Queues one reference for [`drain`]. After shutdown the reference points
/// into freed memory and is forgotten instead.
pub(crate) fn release_later(zv: ManuallyDrop<Zval>) {
    // SAFETY: reading the type flags of a zval we own.
    let counted =
        unsafe { u32::from(zv.u1.v.type_flags) & ext_php_rs::ffi::IS_TYPE_REFCOUNTED != 0 };
    if counted {
        let _ = ENGINE.try_with(|e| {
            if e.alive.get() {
                e.releases.borrow_mut().push(zv);
            }
        });
    }
}

/// Releases queued references, each under its own guard (a release can run
/// a destructor). Stops while PHP must not be entered; the queue is kept for
/// the next drain. An exception thrown by a destructor is left in
/// `EG(exception)` for the caller, as PHP itself does.
pub(crate) fn drain() {
    loop {
        if !can_enter() {
            return;
        }
        let Some(mut zv) = ENGINE.with(|e| e.releases.borrow_mut().pop()) else {
            return;
        };
        let zv_ptr: *mut zval = &raw mut *zv;
        // SAFETY: we own this reference; `zv` lives in this frame.
        let _ = guard(|| unsafe { zval_ptr_dtor(zv_ptr) });
    }
}

/// What PHP threw, taken out of `EG(exception)`.
pub(crate) enum Thrown {
    /// `exit()` / `die()`, with `EG(exit_status)`.
    Exit(i32),
    Exception {
        object: ZvalHandle,
        class: String,
        message: String,
        file: String,
        line: u32,
    },
}

impl Thrown {
    pub(crate) fn into_php_error(self) -> PhpError {
        match self {
            Self::Exit(status) => PhpError::Exit(status),
            Self::Exception {
                class,
                message,
                file,
                line,
                ..
            } => PhpError::Exception {
                class,
                message,
                file,
                line,
            },
        }
    }
}

/// Takes the pending exception, if any. An `exit()` below a native function
/// is parked for that function to rethrow (see [`run_native`]); one at the
/// top is released, and either way reported as [`Thrown::Exit`].
pub(crate) fn take_thrown() -> Option<Thrown> {
    // SAFETY: plain access to the executor globals on the engine's thread.
    // Taking EG(exception) moves its reference to us; the current frame is
    // internal (or none), so no opline needs restoring, as zend_clear_exception
    // shows.
    let obj = unsafe {
        let eg = ext_php_rs_executor_globals();
        let obj = (*eg).exception;
        if obj.is_null() {
            return None;
        }
        (*eg).exception = ptr::null_mut();
        obj
    };
    // SAFETY: `obj` is the live object EG(exception) held.
    if unsafe { zend_is_unwind_exit(obj) || zend_is_graceful_exit(obj) } {
        // SAFETY: as above.
        let status = unsafe { (*ext_php_rs_executor_globals()).exit_status };
        ENGINE.with(|e| {
            if e.native_depth.get() > 0 && e.pending_exit.get().is_null() {
                e.pending_exit.set(obj);
            } else {
                // SAFETY: our reference, adopted and queued for release.
                drop(unsafe { ZvalHandle::adopt_object(obj) });
            }
        });
        return Some(Thrown::Exit(status));
    }
    // SAFETY: we own the reference just taken from EG(exception).
    let object = unsafe { ZvalHandle::adopt_object(obj) };
    let (class, message, file, line) = describe_throwable(obj);
    Some(Thrown::Exception {
        object,
        class,
        message,
        file,
        line,
    })
}

/// Drops whatever is left in `EG(exception)` after a bailout. A leftover
/// exception would make every later `zend_call_function` return at once.
fn discard_exception() {
    // SAFETY: plain access to the executor globals; the reference moves to
    // the handle, which queues its release.
    unsafe {
        let eg = ext_php_rs_executor_globals();
        let obj = (*eg).exception;
        if !obj.is_null() {
            (*eg).exception = ptr::null_mut();
            drop(ZvalHandle::adopt_object(obj));
        }
    }
}

/// Rethrows an exception previously taken with [`take_thrown`], unchanged.
pub(crate) fn rethrow(object: ZvalHandle) {
    if let Some(obj) = object.into_object() {
        // SAFETY: hands our reference to EG(exception) from inside a native
        // function, which is how internal functions throw.
        unsafe { zend_throw_exception_internal(obj) };
    }
}

/// Class, message, file and line of a Throwable.
fn describe_throwable(obj: *mut zend_object) -> (String, String, String, u32) {
    // SAFETY: the caller holds a reference to `obj`.
    let class = unsafe { class_name(obj) };
    let message = read_property(obj, c"message")
        .and_then(|v| match v {
            Value::Str(s) => Some(s),
            Value::Bytes(b) => Some(String::from_utf8_lossy(&b).into_owned()),
            _ => None,
        })
        .unwrap_or_default();
    let file = match read_property(obj, c"file") {
        Some(Value::Str(s)) => s,
        _ => String::new(),
    };
    let line = match read_property(obj, c"line") {
        Some(Value::Int(n)) => u32::try_from(n).unwrap_or(0),
        _ => 0,
    };
    (class, message, file, line)
}

/// Reads one property of an object we hold a reference to.
///
/// Message, file and line are protected properties declared by Exception
/// and Error; reading them with the object's own class as scope is allowed
/// and normally returns the property slot itself. If the engine fills the
/// `rv` buffer instead, the value is read and released while the buffer is
/// still alive.
fn read_property(obj: *mut zend_object, name: &CStr) -> Option<Value> {
    let mut rv = ManuallyDrop::new(Zval::new());
    rv.u1.type_info = ext_php_rs::ffi::IS_UNDEF;
    let rv_ptr: *mut zval = &raw mut *rv;
    let found = guard(|| {
        // SAFETY: `obj` is live; `rv` is zval storage owned by this frame.
        unsafe {
            zend_read_property(
                (*obj).ce,
                obj,
                name.as_ptr(),
                name.to_bytes().len(),
                true,
                rv_ptr,
            )
        }
    })
    .ok()
    .filter(|zv| !zv.is_null());
    // SAFETY: `found` is either a slot of the live object or `rv`, which is
    // still alive here; reading runs no PHP code.
    let value = found.and_then(|zv| unsafe { convert::to_value(zv) }.ok());
    release_later(rv);
    value
}

/// The name of an object's class.
///
/// # Safety
///
/// `obj` is a live object.
pub(crate) unsafe fn class_name(obj: *mut zend_object) -> String {
    // SAFETY: a live object has a class with a name.
    unsafe { String::from_utf8_lossy(convert::str_bytes((*(*obj).ce).name)).into_owned() }
}

/// The last error PHP recorded, as the fatal error a bailout came from.
fn fatal_error(interrupted: Interrupted) -> PhpError {
    if let Interrupted::Refused = interrupted {
        return PhpError::Fatal {
            message: "PHP is unwinding from a fatal error or exit() and cannot run code".to_owned(),
            file: String::new(),
            line: 0,
        };
    }
    const FATAL: u32 =
        E_ERROR | E_CORE_ERROR | E_COMPILE_ERROR | E_USER_ERROR | E_RECOVERABLE_ERROR | E_PARSE;
    // SAFETY: plain reads of the core globals on the engine's thread; the
    // strings are owned by PHP until the next error replaces them.
    unsafe {
        let pg = ext_php_rs_process_globals();
        let kind = u32::try_from((*pg).last_error_type).unwrap_or(0);
        if kind & FATAL == 0 || (*pg).last_error_message.is_null() {
            return PhpError::Fatal {
                message: "PHP bailed out".to_owned(),
                file: String::new(),
                line: 0,
            };
        }
        let text = |s: *mut zend_string| {
            if s.is_null() {
                String::new()
            } else {
                String::from_utf8_lossy(convert::str_bytes(s)).into_owned()
            }
        };
        PhpError::Fatal {
            message: text((*pg).last_error_message),
            file: text((*pg).last_error_file),
            line: u32::try_from((*pg).last_error_lineno).unwrap_or(0),
        }
    }
}

/// Why [`call_php`] did not return a value.
pub(crate) enum CallError {
    Thrown(Thrown),
    Interrupted(Interrupted),
    Convert(String),
}

impl CallError {
    fn into_php_error(self) -> PhpError {
        match self {
            Self::Thrown(thrown) => thrown.into_php_error(),
            Self::Interrupted(interrupted) => fatal_error(interrupted),
            Self::Convert(why) => PhpError::Conversion(why),
        }
    }

    /// The message a hook callback reports for this failure.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Thrown(Thrown::Exit(status)) => format!("PHP code called exit({status})"),
            Self::Thrown(Thrown::Exception {
                class,
                message,
                file,
                line,
                ..
            }) => format!("{class}: {message} in {file}:{line}"),
            Self::Interrupted(interrupted) => fatal_error(*interrupted).to_string(),
            Self::Convert(why) => why.clone(),
        }
    }
}

/// Calls the PHP callable `callable` with `args`.
///
/// Called from Rust with no PHP code running (`EG(current_execute_data)` is
/// null), `zend_call_function` turns an uncaught exception into a fatal
/// error, as it must for `{main}`. To get the exception back instead, the
/// call is made from a dummy frame of an internal function, which is what
/// calling from inside a native function looks like.
pub(crate) fn call_php(callable: *mut zval, args: &[Value]) -> Result<Value, CallError> {
    // Owned here; the guard only sees pointers.
    let mut params: Vec<ManuallyDrop<Zval>> = args
        .iter()
        .map(|_| ManuallyDrop::new(Zval::new()))
        .collect();
    let params_ptr: *mut zval = params.as_mut_ptr().cast();
    let count = u32::try_from(params.len()).unwrap_or(u32::MAX);
    let mut retval = ManuallyDrop::new(Zval::new());
    let retval_ptr: *mut zval = &raw mut *retval;
    let dummy_func = ENGINE.with(|e| e.dummy_func.get());

    let called = guard(|| -> Result<bool, Unconvertible> {
        // SAFETY: `params` and `retval` are zval storage owned by the
        // enclosing frame; `params` has `count` slots (ManuallyDrop<Zval> is
        // a transparent Zval). The dummy frame lives in this frame for the
        // duration of the call; a bailout resets EG(current_execute_data)
        // itself.
        unsafe {
            for (i, arg) in args.iter().enumerate() {
                convert::write_value(params_ptr.add(i), arg)?;
            }
            let eg = ext_php_rs_executor_globals();
            let mut dummy = MaybeUninit::<zend_execute_data>::zeroed();
            let installed = (*eg).current_execute_data.is_null();
            if installed {
                (*dummy.as_mut_ptr()).func = dummy_func;
                (*eg).current_execute_data = dummy.as_mut_ptr();
            }
            let result = _call_user_function_impl(
                ptr::null_mut(),
                callable,
                retval_ptr,
                count,
                params_ptr,
                ptr::null_mut(),
            );
            if installed {
                (*eg).current_execute_data = ptr::null_mut();
            }
            Ok(result == ZEND_RESULT_CODE_SUCCESS)
        }
    });

    // SAFETY: `retval` is null or what the callable returned.
    let value = unsafe { convert::to_value(retval_ptr) };
    for param in params {
        release_later(param);
    }
    release_later(retval);
    drain();

    match called {
        Err(interrupted) => {
            discard_exception();
            return Err(CallError::Interrupted(interrupted));
        }
        Ok(Err(why)) => return Err(CallError::Convert(why.to_string())),
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            return Err(CallError::Convert(
                "PHP refused the call (the executor is inactive)".to_owned(),
            ));
        }
    }
    if let Some(thrown) = take_thrown() {
        return Err(CallError::Thrown(thrown));
    }
    value
        .map_err(|e: ConvertError| CallError::Convert(format!("the result cannot reach Rust: {e}")))
}

/// Looks up a function in the engine's function table.
///
/// # Safety
///
/// A request is open.
unsafe fn find_function(name: &str) -> *mut zend_function {
    // SAFETY: function_table is a live hash of IS_PTR zvals.
    unsafe {
        let table = (*ext_php_rs_executor_globals()).function_table;
        let zv = zend_hash_str_find(table, name.as_ptr().cast(), name.len());
        if zv.is_null() {
            ptr::null_mut()
        } else {
            (*zv).value.func
        }
    }
}

/// A zval view of an array pointer, for [`convert::to_value`].
fn table_as_zval(table: *mut ext_php_rs::ffi::zend_array) -> ManuallyDrop<Zval> {
    let mut zv = ManuallyDrop::new(Zval::new());
    zv.value.arr = table;
    // Not marked refcounted: this view holds no reference of its own.
    zv.u1.type_info = ext_php_rs::ffi::IS_ARRAY;
    zv
}

/// `s` as a single-quoted PHP string literal.
fn php_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\\' || c == '\'' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Sends PHP's output to stdout or stderr.
extern "C" fn ub_write(data: *const c_char, len: usize) -> usize {
    if len == 0 || data.is_null() {
        return 0;
    }
    // SAFETY: PHP passes a non-null buffer of `len` bytes that outlives the call.
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) };
    let written = if OUTPUT_TO_STDERR.load(Ordering::SeqCst) {
        std::io::stderr().lock().write_all(bytes)
    } else {
        std::io::stdout().lock().write_all(bytes)
    };
    match written {
        Ok(()) => len,
        Err(_) => 0,
    }
}

extern "C" fn flush(_server_context: *mut c_void) {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
}

/// Sends PHP's log messages (startup errors, `error_log()`) to stderr.
extern "C" fn log_message(message: *const c_char, _syslog_type: c_int) {
    if message.is_null() {
        return;
    }
    // SAFETY: PHP passes a NUL-terminated string that outlives the call.
    let message = unsafe { CStr::from_ptr(message) };
    eprintln!("{}", message.to_string_lossy());
}
