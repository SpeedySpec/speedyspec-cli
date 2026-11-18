//! Embedded, statically linked PHP for speedyspec.
//!
//! [`Php::boot`] starts the interpreter once per process. PHP is
//! process-global and non-thread-safe, so a [`Php`] stays on the thread that
//! booted it.

mod runtime;

pub use runtime::{Php, PhpError};

/// The PHP version linked into this binary, from the repository's
/// `php-version` file.
pub const PHP_VERSION: &str = env!("SPEEDYSPEC_PHP_VERSION");
