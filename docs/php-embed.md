# Embedding PHP

speedyspec links a static PHP interpreter into its own binary and runs PHP on
the main thread. PHP code calls back into functions implemented in Rust. This
page records how that is built and why, as established by the first proof
(`crates/speedyspec-php`, test `tests/php.rs`).

## Versions

| Piece | Version | Pinned in |
| --- | --- | --- |
| PHP | 8.5.12 (non-thread-safe) | `php-version` (the only place) |
| static-php-cli (`spc`) | 2.8.6, nightly build published 2026-08-22 | `scripts/build-php.sh` (SHA-256 per host) |
| ext-php-rs | 0.16.5 (`=0.16.5`, feature `embed`) | `crates/speedyspec-php/Cargo.toml` |
| Rust | 1.99 | `Cargo.toml` (`rust-version`), `scripts/linux.sh` (`rust:1.99-alpine`) |

**Why PHP 8.5.12.** It is the newest PHP release as of 2026-11-17 (php.net
lists 8.5.12, 2026-10-21, as the latest 8.5). PHP 8.6.0 is due 2026-11-19 and
is not out yet. static-php-cli documents support for 8.1 through 8.5 and
accepts a full `x.y.z` version in `craft.yml`. ext-php-rs 0.16.5 supports PHP
API versions from 8.1 through 8.6 (`ext-php-rs-build` 0.2.1 knows API
20250925 for 8.5). Moving to 8.6 later means changing `php-version` and
rerunning `scripts/build-php.sh`, once static-php-cli supports 8.6.

**`spc` checksums.** `dl.static-php.dev` only publishes `spc` on a mutable
`nightly` channel, so the checksum in the script is what pins it. If upstream
replaces the nightly binary the script fails with a checksum mismatch; re-pin
deliberately after checking the new version (`spc --version`).

| Host | File | SHA-256 |
| --- | --- | --- |
| macOS arm64 | `spc-macos-aarch64` | `9eba2e6e0d1aebabd6ff8efc6711dd0bad335ba41e29ce36e06ea5a6c5d57731` |
| Linux x86_64 | `spc-linux-x86_64` | `c138f3e4a68479597e11ca14d5209346c54758ccd09005e7a6a6f1e63b7f9f71` |
| Linux arm64 | `spc-linux-aarch64` | `26f483093b97da2925e4e993036c59509eab28656346b505d2d0a11271faae99` (pinned, never run) |

## Building PHP: the recipe

`scripts/build-php.sh` (run it from anywhere; it works relative to the repo):

1. Writes `craft.yml` (committed, regenerated on every run) from `php-version`:

   ```yaml
   php-version: "8.5.12"
   extensions: "ctype,curl,filter,mbstring,openssl,phar,tokenizer,zip,zlib"
   sapi:
     - embed
     - cli
   craft-options:
     doctor: false
   ```

2. Skips the build if `.php-build/buildroot/lib/libphp.a` exists and
   `.php-build/buildroot/.craft.yml` matches `craft.yml` byte for byte (that
   is how a CI cache hit avoids a rebuild).
3. Checks that the host has the build tools (see "Host tools" below) and
   fails with an install hint if not, unless
   `SPEEDYSPEC_ALLOW_HOST_INSTALL=1`.
4. Downloads `spc` for the host from
   `https://dl.static-php.dev/static-php-cli/spc-bin/nightly/` into
   `.php-build/spc` and checks its SHA-256.
5. With `SPEEDYSPEC_ALLOW_HOST_INSTALL=1`, runs `spc doctor --auto-fix`.
   Otherwise, if no `pkg-config` is on `PATH` or in `.php-build/pkgroot`,
   runs `spc install-pkg pkg-config`, which builds one into
   `.php-build/pkgroot` (inside the repository).
6. Runs `spc craft <repo>/craft.yml` inside `.php-build/`. `craft` runs
   `spc download --with-php=8.5.12 ...` and
   `spc build ... --build-embed --build-cli`. `craft-options: doctor: false`
   stops it from running its own `spc doctor --auto-fix` first (static-php-cli's
   `CraftCommand` runs doctor only when `craft-options.doctor` is true, which
   is the default).
7. Writes `.php-build/buildroot/link-flags.txt` from
   `spc spc-config <extensions> --libs`, then stamps `.craft.yml`.
8. Fails unless `.php-build/buildroot/lib/libphp.a` exists.

### Host tools

By default `scripts/build-php.sh` installs and changes nothing outside the
repository. static-php-cli's `spc doctor` installs missing tools with
Homebrew or the distro package manager, and so does plain `spc doctor`:
without `--auto-fix` it asks "Do you want to fix it?", and when there is no
terminal to answer it goes ahead. (`--auto-fix=never` only reports, and it
still exits 0, so it cannot gate a build.) That is how this proof's first
build installed `bison` 3.8.2 with Homebrew on the development Mac. The
script therefore never lets spc run doctor unless asked. It checks for the
same commands itself, read-only:

- macOS: `curl make re2c flex git autoconf automake tar libtool unzip xz
  gzip bzip2 cmake glibtoolize`, plus bison 3 or newer (on `PATH` or in
  Homebrew's keg-only `/opt/homebrew/opt/bison/bin`; macOS's own bison is
  2.3). Hint:
  `brew install autoconf automake bison bzip2 cmake flex git libtool make re2c xz`.
- Linux: `curl make re2c flex git autoconf automake tar unzip xz gzip bzip2
  cmake gcc g++ patch libtoolize perl which`, plus bison 3+. Hint: use the
  package manager, or build in the container (`scripts/linux.sh`). Doctor
  also checks things such as Linux headers and, on non-Alpine hosts, the musl
  toolchain, which this list does not cover; there spc fails during the build
  instead.

`SPEEDYSPEC_ALLOW_HOST_INSTALL=1` skips the check and runs
`spc doctor --auto-fix`, which may install packages on the machine.
`scripts/linux.sh` sets it inside its container, which is thrown away after
each run. `ci.yml` sets it for the build step, because CI runners are
throwaway too.

Notes on the settings:

- **`sapi: embed`** builds `libphp.a`. static-php-cli's `env.ini` defaults
  `SPC_CMD_VAR_PHP_EMBED_TYPE="static"`, and PHP is configured with
  `--enable-embed=static --enable-shared=no --enable-static=yes`.
- **`cli` is a build-time tool only.** ext-php-rs's build script runs `php -i`
  (through `ext-php-rs-build`) to read the PHP API version and whether the
  build is thread-safe, so it needs a `php` binary of the same build. Nothing
  ships it.
- **Non-thread-safe.** That is static-php-cli's default; ZTS needs an explicit
  `--enable-zts` build option, which `craft.yml` does not set. `php -i`
  reports `Thread Safety => disabled`.
- **Debug off, symbols stripped** are static-php-cli defaults.
- static-php-cli configures PHP with
  `--with-config-file-path=/usr/local/etc/php` and a matching scan dir. That
  does not matter: the runtime tells PHP to ignore php.ini (below).

## Pointing ext-php-rs at the static build

ext-php-rs's build script finds PHP through the `PHP` and `PHP_CONFIG`
environment variables, falling back to `php` / `php-config` on `PATH`, which
would be the system PHP (8.4 on the development machine). Cargo runs that
build script before ours, and one build script cannot set another's
environment, so:

- `.cargo/config.toml` sets `PHP` and `PHP_CONFIG` (relative to the repo) to
  `.php-build/buildroot/bin/php` and `bin/php-config`. Variables already in
  the environment win.
- `crates/speedyspec-php/build.rs` reads `SPEEDYSPEC_PHP_PREFIX` (default
  `<workspace>/.php-build/buildroot`) and **fails the build** unless `PHP` and
  `PHP_CONFIG` resolve to `<prefix>/bin/php` and `<prefix>/bin/php-config`,
  and unless `php-config --version` equals `php-version`. Headers, generated
  bindings and the linked library therefore always come from one PHP build.
- `build.rs` exports the version as `SPEEDYSPEC_PHP_VERSION`, which becomes
  `speedyspec_php::PHP_VERSION`.

To use a PHP build elsewhere, set all three of `SPEEDYSPEC_PHP_PREFIX`, `PHP`
and `PHP_CONFIG`.

## Link flags

`php-config --libs` is incomplete for a static-php-cli build: on macOS it
reports `-lresolv -lz -lcurl -lssl -lcrypto -lzip` and omits `libxml2`,
`libiconv`, `libcharset` and the system frameworks that curl needs.
`spc spc-config --libs` reports the full set, so `build.rs` reads that
(`link-flags.txt`) instead. For every `-lNAME` it emits
`rustc-link-lib=static:-bundle=NAME` when `<prefix>/lib/libNAME.a` exists,
and a plain system library otherwise; `-framework X` becomes
`rustc-link-lib=framework=X`. `-bundle` keeps the archives out of the rlib;
they are linked once, into the final binary.

ext-php-rs's `embed` feature additionally emits `rustc-link-lib=php`; since the
prefix only contains `libphp.a`, that resolves to the same static archive.

**macOS arm64** (`link-flags.txt`):

```
-L<prefix>/lib -lphp -lzip -lcurl -lssl -lcrypto -lxml2 -lm -lz -liconv
-lcharset -framework CoreFoundation -framework CoreServices
-framework SystemConfiguration -lresolv -lc
```

Static: php, zip, curl, ssl, crypto, xml2, z, iconv, charset. Dynamic (system,
always present): libSystem, libresolv, CoreFoundation, CoreServices,
SystemConfiguration.

**Linux x86_64 musl** (`link-flags.txt`):

```
-L<prefix>/lib -lphp -lzip -lcurl -lssl -lcrypto -lz -ldl -lpthread -lm -lc
```

All of these resolve to static archives (the prefix's, or musl's own `libc.a`,
of which `libdl`/`libpthread`/`libm` are stubs), and the target's default
`crt-static` makes rustc link a `static-pie` executable. One extra library:
`build.rs` adds `-lgcc` (static) on Linux, because PHP's opcache JIT uses
GCC's CPU-detection symbols (`__cpu_model`, `__cpu_indicator_init_local`,
`__cpu_features2`) from libgcc. gcc links libgcc by default, so spc does not
list it, but rustc links with `-nodefaultlibs` and the link fails without it.

**Host tools in the musl container.** On Alpine the host target is also musl
with `crt-static` on, so build scripts are static too, and a static musl
binary cannot `dlopen`. bindgen (inside ext-php-rs's build script) dlopens
libclang and failed with `Dynamic loading not supported`. `scripts/linux.sh`
fixes this without touching the target: it sets
`CARGO_BUILD_TARGET=x86_64-unknown-linux-musl` (so cargo passes `--target`
only for target artifacts) and `RUSTC_WRAPPER=rustc-host-dynamic`, a
four-line wrapper baked into its image that adds
`-C target-feature=-crt-static` to rustc invocations without `--target`,
i.e. build scripts and proc macros. Test binaries stay fully static. Their
path is therefore `target-linux/x86_64-unknown-linux-musl/debug/...`.

## Starting PHP and registering native code

ext-php-rs's `Embed::run` uses PHP's stock embed SAPI, which offers no way to
add a module, and its `php_embed_module` cannot ignore php.ini without
patching. speedyspec therefore starts the engine itself with a minimal SAPI,
the same sequence ext-php-rs's own SAPI tests use (`src/runtime.rs`):

```rust
let sapi = SapiBuilder::new("speedyspec", "SpeedySpec")
    .ub_write_function(ub_write)        // PHP output -> stdout, or stderr (Php::redirect_output)
    .flush_function(flush)
    .log_message_function(log_message)  // PHP log -> stderr
    .php_ini_ignore(1)                  // never read php.ini, PHPRC or PHP_INI_SCAN_DIR
    .build()?;
ext_php_rs_sapi_startup();
sapi_startup(sapi);
(*sapi).ini_entries = INI_ENTRIES.as_ptr();          // after sapi_startup, which clears it
php_module_startup(sapi, hook_class::module_entry()); // <- registers our module
php_request_startup();
CG(skip_shebang) = true;                              // as the CLI does
```

`Php::boot(BootOptions { hooks, preload })` then evaluates the PHP source of
`SpeedySpec\Hooks\HookException` (a plain
`class HookException extends \RuntimeException {}`; defining it in PHP was
simpler than a native class with a parent) and the `preload` files, in
order, before anything else runs.

**Native functions and classes** are registered as one PHP module, exactly
like a built-in extension. `#[php_module]` (in `src/hook_class.rs`)
generates `get_module() -> *mut zend_module_entry` from a `ModuleBuilder`;
`php_module_startup` hands that module to PHP, which registers its functions
during startup and runs its MINIT, where the class is registered:

```rust
#[php_module]
#[php(startup = startup)]            // our MINIT hook, run before ext-php-rs's own
pub fn module(module: ModuleBuilder) -> ModuleBuilder {
    module
        .function(add_signature("add_action", hook_add))   // FunctionBuilder + raw handler
        // ... add_filter, remove_*, has_*, do_action, apply_filters, did_action
}

fn startup(_ty: i32, _module_number: i32) -> i32 {
    ClassBuilder::new("SpeedySpec\\Hooks\\Hook")
        .flags(ClassFlags::Final)
        .method(add_signature("add", hook_add), MethodFlags::Public | MethodFlags::Static)
        // ... remove, has, run, filter, ran
        .registration(|_ce, arg_info| std::mem::forget(arg_info)) // lives as long as the class
        .register()
    // 0 on success
}
```

The handlers are hand-written `extern "C" fn(&mut ExecuteData, &mut Zval)`
rather than `#[php_function]` / `#[php_impl]`, because every native entry
must go through `runtime::native_entry` (see "Bailouts, exceptions and exit()"
below). The global aliases and the methods share one handler each. Arguments
are read from the frame directly and type errors are thrown as PHP's own
`TypeError`, `ArgumentCountError` and `ValueError`.

Parameter types: `$hook` must be a string, always. `$priority` (`add`,
`remove` and their aliases) and `$acceptedArgs` (`add` and its aliases)
accept any integer-valued number whatever the caller's `strict_types`, as
WordPress does: an int, a float with an integral value (`20.0`), or a
numeric string with an integral value (`'20'`, `' 20'`, `'-5'`, `'20.0'`),
parsed by PHP's own `_is_numeric_string_ex` (surrounding whitespace allowed,
trailing data not). `'20'` and `20` are the same priority. Anything else
(non-numeric or partly numeric strings, fractions, `null`, bools, arrays,
objects, values outside 64 bits) is a `TypeError` naming the parameter, as is
a priority outside the registry's `i32`. A negative `$acceptedArgs` is a
`ValueError`.

The macro route also works, and was checked in Task 1 with a throwaway class
(`#[php_class]` + `#[php_impl]`, `module.class::<T>()`): method names are then
converted to camelCase by default (`add_two` -> `addTwo`) unless
`#[php(name = "...")]` overrides them, and
`ReflectionClass::getExtensionName()` reports `speedyspec-php`, the crate's
package name.

Constraints of this mechanism:

- `php_module_startup` takes exactly one additional module, so everything
  native lives in one `#[php_module]` (in `speedyspec-php`).
- `#[php_module]` exports an unmangled `get_module` symbol; there can be only
  one in the binary.
- The engine starts once per process (`Php::boot` returns `PhpError::Boot` on
  a second call) and is not `Send`: the build is non-thread-safe.

**INI.** `php_ini_ignore = 1` stops PHP reading any php.ini, `PHPRC` and
`PHP_INI_SCAN_DIR`. Verified: with `PHPRC` and `PHP_INI_SCAN_DIR` pointing at
a directory holding a `php.ini`, the static `php` CLI loads it but the test
binary still sees `php_ini_loaded_file() === false`. The SAPI's fixed INI
entries mirror PHP's CLI SAPI (`display_errors=1`, `html_errors=0`,
`implicit_flush=1`, `output_buffering=0`, `max_execution_time=0`,
`log_errors=0`) except `register_argc_argv=0`: under any SAPI other than
`cli`, PHP 8.5 would derive `$_SERVER['argv']` from the query string and
emit a deprecation, so `Php::run_file` sets `$argv`, `$argc`,
`$_SERVER['argv']` and `$_SERVER['argc']` itself.

**Running code.** All of these compile and execute in the open request and
return PHP's errors as `PhpError` values:

- `Php::eval(label, code)` compiles `code` as statements (no `<?php`) with
  `zend_compile_string`, as `zend_eval_stringl` does, but keeps the op array
  so a top-level `return` becomes the result. `label` stands in for the file
  name in errors.
- `Php::require(path)` evaluates `return require '<absolute path>';`, so it
  never consults `include_path`, and returns the file's return value.
- `Php::run_file(path, argv)` sets `$argv` (`argv[0]` is `path`), `$argc` and
  `$_SERVER['argv'/'argc']`, then `require`s the file: `Ok(0)` when it ends,
  `Ok(n)` for `exit(n)`, `Err` for an uncaught exception or fatal error. It
  no longer uses `php_execute_script`, which reports an uncaught exception by
  printing it and offers no structured error. A path that is not valid UTF-8
  is `PhpError::NonUtf8Path`, a missing one `ScriptNotFound`.
- `Php::call(callable, args)` calls through `_call_user_function_impl`.

An uncaught exception is `PhpError::Exception { class, message, file, line }`
(read from the Throwable's properties); `exit()`/`die()` is `Exit(status)`; a
fatal error (a bailout) is `Fatal { message, file, line }` from PHP's last
recorded error. PHP itself still displays fatal errors on the output, as the
CLI does with `display_errors=1`; exceptions it does not see as uncaught, so
it prints nothing for them. The engine keeps working after a fatal error, as
PHP's interactive shell does after catching one (a bailout leaves the VM
frames of the abandoned code allocated until the request ends, and PHP
disables the cycle collector after a bailout; both are per fatal error, not
per call).

A call from Rust when no PHP code is running has no current frame, and
`zend_call_function` then turns an uncaught exception into a fatal error, as
it must for `{main}`. The bridge therefore calls from a zeroed dummy frame
whose `func` is an internal function (`apply_filters`), which is what a call
from inside a native function looks like; the exception stays in
`EG(exception)` and is taken out by Rust.

## Bailouts, exceptions and exit() across the Rust boundary

A PHP fatal error is a bailout: a `longjmp` to the innermost `zend_try`. A
`longjmp` over a Rust frame skips that frame's destructors (and is undefined
behaviour if any of them would have run). Hook callbacks make this real:
PHP calls `Hook::run` (Rust), which fires a PHP callback (PHP), which can hit
a fatal error, call `exit()` or throw. The rule `src/runtime.rs` follows:

1. **Every entry into PHP goes through `guard`.** It is a plain `zend_try`
   (ext-php-rs `try_catch`, not `try_catch_first`, so it saves and restores
   the enclosing `EG(bailout)` and nests safely) around a closure that
   captures only references and raw pointers. Whatever the call works on
   (argument zvals, the return zval, the op array, strings) is owned by the
   caller, outside the closure, and released normally afterwards. A bailout
   therefore only ever skips the closure's own frame, which owns nothing.
   Converting a Rust value to a zval allocates PHP memory, which can bail out
   on `memory_limit`, so it runs inside the guard too, with frames that own
   nothing (`convert::write_value`).
2. **Every native function PHP calls goes through `native_entry`.** When a
   guard inside it catches a bailout, it sets a flag instead of continuing;
   the method then returns normally through all its Rust frames, dropping
   everything, and only then does `native_entry`, in a frame that owns
   nothing, re-raise the bailout to the PHP code below. While the flag is set
   no guard enters PHP. Releasing values at the end of a native call is part
   of that call, so a destructor that bails out there is re-raised too.
3. **A Rust panic never becomes a PHP error.** A panic inside a guarded
   closure is resumed as soon as the closure has unwound. A panic in a native
   function (say a Rust hook callback fired from PHP) cannot unwind through
   PHP's C frames: `native_entry` catches it, parks the payload, and carries
   it through PHP as a bailout; the next guard below resumes the same
   payload in Rust (`std::panic::resume_unwind`).
4. **Rust never releases a PHP value from `Drop`.** Releasing can run a
   destructor or the cycle collector, that is PHP code. Dropping a handle to
   a PHP value queues the reference; the queue is drained, each value under
   its own guard, at the end of every native call and every `Php` method and
   PHP callback.
5. **Exceptions and `exit()` are values, not jumps.** An exception is left in
   `EG(exception)` by PHP and taken out by the code that called in. A PHP
   callback's exception becomes that callback's `CallbackError`; when the
   firing was started from PHP the original object is kept and rethrown,
   unchanged, to the PHP caller. `exit()` throws PHP's internal unwind-exit
   object; below a native function it is parked and rethrown by that
   function, so the calling script unwinds exactly as for its own `exit()`
   (`finally` blocks do not run). Fired from Rust, it is the callback's error
   (`PHP code called exit(n)`).

6. **Fibers: the unwinding state is per execution context.** Hook callbacks
   may create, start, resume, throw into and finish fibers, and a callback
   running inside a fiber may call `Fiber::suspend()`: the firing pauses
   with the fiber and continues from the same callback, with the remaining
   callbacks, when the fiber is resumed. Each fiber has its own C stack, and
   on one stack native calls still nest strictly; only across stacks they do
   not. So the state the rules above rely on is kept per context (the main
   context and each fiber, i.e. each `zend_fiber_context`): how many of our
   native calls are on that stack, the pending bailout re-raise, the parked
   `exit()`/graceful-exit object, and the firing frames that hold a PHP
   callback's exception for its PHP caller. Fiber observers registered from
   our MINIT (`zend_observer_fiber_{init,switch,destroy}_register`; the
   observer lists exist from `zend_observer_startup`, which runs before
   module MINITs, and registering them changes nothing else) swap that state:
   on a switch, which PHP announces on the outgoing stack just before
   jumping, the outgoing context's record is saved under its address and the
   incoming one's loaded (fresh for a new fiber); a destroyed context's
   record is freed, releasing what it held; a new context at a reused address
   drops any stale record. PHP keeps `EG(bailout)`, the VM stack and
   `EG(current_execute_data)` per fiber itself, so a bailout never jumps
   across stacks: inside a fiber it is caught by the fiber's own `zend_try`
   and re-raised by PHP in the context that resumed the fiber, where the
   guards above handle it (a Rust panic carried as a bailout follows the
   same path and is resumed there). `exit()` in a fiber's callback reaches
   the resumer as PHP's own error transfer and ends the script.
   `Fiber::suspend()` from a callback with no fiber on the stack (fired from
   Rust or from top-level code) is PHP's own `FiberError`, an ordinary
   callback exception.

   **Parked Rust frames.** While a fiber is suspended inside a hook, the Rust
   frames of that firing (the native call, the registry, the callback
   trampoline) sit on the fiber's stack. They hold no `RefCell` borrow, lock
   or thread-local borrow across a call into PHP (every borrow in the bridge
   ends before PHP is entered, and `speedyspec-hooks-memory` snapshots the
   entries before calling any callback), so other contexts can use the
   registry and the engine freely. When PHP destroys a suspended fiber
   (last reference gone, cycle collection, or shutdown) it resumes it with
   its internal graceful-exit object: `Fiber::suspend()` throws it, it
   unwinds the callback, the trampoline parks it like an `exit()` (it is not
   reported as one: `PhpError`/`CallbackError` say the fiber was destroyed),
   and the native call rethrows it, so the frames return normally and
   release what they hold. After a fatal error PHP marks every existing
   object destructed; a fiber suspended at that time is later freed without
   being resumed, and its stack, with our frames on it, is abandoned: never
   run again, its Rust values leaked. None of them depends on its destructor
   for soundness (no borrow guards, pins or locks), so this is a leak, not
   undefined behaviour. Such a fiber can still be resumed normally while it
   is referenced.

   **What is per context and what is not.** Per context: the native depth,
   the pending re-raise, the parked exit object, the firing frames (above).
   Process-wide, on purpose: the release queue (a reference may be released
   from any context, and draining runs under a guard wherever it happens, so
   a destructor that destroys a fiber there is fine); the parked panic (at
   most one is in flight, and it travels with the bailout from context to
   context); `poisoned()` (a property of the engine); the entry counter;
   `alive`; the dummy-frame function; the hook registry and the
   `HookException` class; the owner stamp (`set_owner` names the plugin for
   every later `Hook::add`, from whichever context, until changed); and the
   output redirection (process stdout/stderr). ext-php-rs's bailout cleanup
   stack is thread-wide too, but the bridge never puts anything on it.

**Poisoned engine.** The engine keeps working after a bailout, as PHP's
interactive shell does, but degraded for the rest of its single request:
`_zend_bailout` turns the cycle collector off (`gc_protect(1)`) and sets
`CG(unclean_shutdown)`, so reference cycles are no longer freed; a fatal
error during compilation leaves that file open (`CG(open_files)`) and its
compiler state behind until the request ends; a fatal error can strike
part-way through an engine operation and leave its structures half-updated;
and PHP marks every object existing at a fatal error as destructed, so their
destructors never run. `Php::poisoned()` is set the first time any bailout is
caught (a fatal error, or a Rust panic carried through PHP) and never
cleared, so the caller can decide whether to carry on. `exit()` does not
poison: in PHP 8 it unwinds as an exception, not a bailout.

**Reference counts.** A PHP object or resource reaching Rust becomes a
`Value::Opaque` holding one counted reference (`convert::ZvalHandle`);
passing it back to PHP copies the zval and adds a reference, so the same
object comes out. PHP callables stored in the registry are held the same way.
The harness checks the balance: an object passed through PHP and Rust
filters many times is destroyed exactly when PHP drops its last reference,
and a thousand more firings, from PHP and from Rust, do not change
`memory_get_usage()`.

**Shutdown.** Dropping `Php`, in this order: (1) drains the release queue;
(2) ends the request with everything still in place (the engine marked
alive, the registry, the per-context records, `HookException`). PHP's
shutdown calls every remaining destructor, and a fiber still suspended
inside a hook is destroyed then, which runs the parked Rust frames to
their end (see rule 6) while everything they touch still exists. The
request then frees every PHP object, including those Rust still holds (PHP
callbacks left in the registry, `Value::Opaque`s kept by Rust code). (3)
Only after that is the engine marked dead: queued and still-held
references, and the records of abandoned fibers, are forgotten, never
released (their memory belonged to the request), and a PHP callback left in
a registry that outlives `Php` returns a `CallbackError` when fired instead
of entering PHP. (4) The module and SAPI shut down.

## Build time and binary size

Measured on the development Mac (Apple Silicon, builds ran with `make -j16`) on 2026-11-17.

| | macOS arm64 (native) | Linux x86_64 musl (Docker, emulated on arm64) |
| --- | --- | --- |
| First `scripts/build-php.sh` (downloads + all libraries + PHP) | 7 min 43 s | 21 min 12 s |
| Rebuild of PHP only (libraries already built) | 2 min 49 s | 8 min 2 s |
| `libphp.a` | 51.3 MB | 57.4 MB |
| First `cargo test -p speedyspec-php --test php` (all deps) | 40 s | about 1 min |
| Test binary, debug | 18.5 MB | 59.4 MB |
| Test binary, release | 16.0 MB (14.3 MB stripped) | 19.7 MB (17.6 MB stripped) |
| `speedyspec` binary, release (`cargo build --release -p speedyspec-cli`) | 16.9 MB (16,924,112 bytes) | not measured |

`env -i ./target/release/speedyspec --version` (empty environment, so no PHP on `PATH`) prints the three version lines on macOS: the
binary needs no PHP on the machine.

Linux x86_64 timings are under QEMU emulation on an arm64 host; a native
x86_64 CI runner should be faster.

## Verification

`cargo test -p speedyspec-php --test php` prints an `ok` line per check (48
since Task 3's third fix round) and exits 0 on both targets.

- **macOS arm64**, native: `otool -L` on the test binary lists only
  `libSystem`, `libresolv` and the CoreFoundation, CoreServices and
  SystemConfiguration frameworks. No libphp, libssl, libcurl or libxml2.
- **Linux x86_64 musl**, `scripts/linux.sh` with `--platform linux/amd64`
  (QEMU on the arm64 host; `uname -m` inside reports `x86_64`): `file`
  reports `ELF 64-bit LSB pie executable, x86-64 ... static-pie linked`, and
  `readelf -d` shows no `NEEDED` entries. musl's `ldd` does not print "not a
  dynamic executable" for static-pie binaries; it prints its own loader line
  instead, so it is not a useful check here. The decisive check: the test
  binary passes when run as the only program in an empty `FROM scratch`
  image (with a tmpfs `/tmp` for the test's stdout capture), with no libc or
  loader present.
- Linux arm64 was not needed and was not run.

## Fallbacks taken

None of the Global Constraints fallbacks was needed: both targets link PHP and
libc statically (macOS always links libSystem dynamically; that is the
platform, not a fallback). Two recipe adjustments were needed on Linux, both
described under "Link flags": `-lgcc` in `build.rs`, and the host-only
dynamic rustc wrapper in `scripts/linux.sh`.

Deviations from the original plan, and why:

- `craft.yml` builds the `cli` SAPI as well as `embed` (ext-php-rs runs
  `php -i` at build time).
- Link flags come from `spc spc-config --libs`, not `php-config --libs`,
  which omits libraries.
- `.cargo/config.toml` exists to point ext-php-rs at the static PHP.
- `register_argc_argv=0`; `run_file` sets `$argv`/`$argc` (and, since Task 3,
  `$_SERVER['argv']`/`['argc']`) itself.

## CI

`.github/workflows/ci.yml` runs on `macos-latest` (arm64) and `ubuntu-latest`
(through `scripts/linux.sh`, so tests run in the same musl container used
locally), caches `.php-build*/spc` and `.php-build*/buildroot` keyed on
`php-version` and `scripts/build-php.sh`, builds PHP (with
`SPEEDYSPEC_ALLOW_HOST_INSTALL=1`), and runs
`cargo test --workspace`. **It has never run**: the repository does not exist
on GitHub yet.
