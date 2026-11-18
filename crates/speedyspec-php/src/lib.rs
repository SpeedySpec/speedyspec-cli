//! Embedded, statically linked PHP for speedyspec.
//!
//! [`Php::boot`] starts the interpreter once per process. PHP is
//! process-global and non-thread-safe, so a [`Php`] stays on the thread that
//! booted it. The hook registry passed in [`BootOptions`] is exposed to PHP as
//! `SpeedySpec\Hooks\Hook` and the WordPress-named functions.

mod convert;
mod hook_class;
mod runtime;

pub use runtime::{BootOptions, Output, Php, PhpError};

/// The PHP version linked into this binary, from the repository's
/// `php-version` file.
pub const PHP_VERSION: &str = env!("SPEEDYSPEC_PHP_VERSION");

/// How many times this process has entered the PHP engine. Every entry the
/// bridge makes (running code, calling a callable, firing a PHP callback,
/// releasing a PHP value) is counted; tests use it to show when PHP was not
/// entered at all.
#[doc(hidden)]
pub fn php_entries() -> u64 {
    runtime::entries()
}
