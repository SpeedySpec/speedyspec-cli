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

1. Downloads `spc` for the host from
   `https://dl.static-php.dev/static-php-cli/spc-bin/nightly/` into
   `.php-build/spc` and checks its SHA-256.
2. Writes `craft.yml` (committed, regenerated on every run) from `php-version`:

   ```yaml
   php-version: "8.5.12"
   extensions: "ctype,curl,filter,mbstring,openssl,phar,tokenizer,zip,zlib"
   sapi:
     - embed
     - cli
   ```

3. Skips the build if `.php-build/buildroot/lib/libphp.a` exists and
   `.php-build/buildroot/.craft.yml` matches `craft.yml` byte for byte (that
   is how a CI cache hit avoids a rebuild).
4. Otherwise runs `spc craft <repo>/craft.yml` inside `.php-build/`. `craft`
   runs `spc doctor --auto-fix`, `spc download --with-php=8.5.12 ...` and
   `spc build ... --build-embed --build-cli`.
5. Writes `.php-build/buildroot/link-flags.txt` from
   `spc spc-config <extensions> --libs`, then stamps `.craft.yml`.
6. Fails unless `.php-build/buildroot/lib/libphp.a` exists.

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
- **`spc doctor --auto-fix` installs missing build tools.** On macOS it uses
  Homebrew (on this machine it installed `bison` 3.8.2) and builds its own
  `pkg-config` under `.php-build/pkgroot`. On Alpine it would `apk add`; the
  image built by `scripts/linux.sh` already has the tools.
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
    .ub_write_function(ub_write)        // PHP output -> process stdout
    .flush_function(flush)
    .log_message_function(log_message)  // PHP log -> stderr
    .php_ini_ignore(1)                  // never read php.ini, PHPRC or PHP_INI_SCAN_DIR
    .build()?;
ext_php_rs_sapi_startup();
sapi_startup(sapi);
(*sapi).ini_entries = INI_ENTRIES.as_ptr(); // after sapi_startup, which clears it
php_module_startup(sapi, get_module());     // <- registers our module
php_request_startup();
```

**Native functions** are registered as a PHP module, exactly like a built-in
extension. `#[php_function]` wraps a Rust function; `#[php_module]` generates
`get_module() -> *mut zend_module_entry` from a `ModuleBuilder`;
`php_module_startup(sapi, get_module())` hands that module to PHP, which
registers its functions during startup:

```rust
#[php_function]
pub fn speedyspec_proof_add(a: i64, b: i64) -> PhpResult<i64> { ... }

#[php_module]
pub fn module(module: ModuleBuilder) -> ModuleBuilder {
    module.function(wrap_function!(speedyspec_proof_add))
}
```

**Native classes** go through the same module. ext-php-rs registers them from
the module's startup (MINIT) hook, which `php_module_startup` runs:

```rust
#[php_class]
#[php(name = "SpeedySpec\\Proof\\Calc")]   // namespaced PHP name
pub struct Calc;

#[php_impl]
impl Calc {
    pub fn add_two(a: i64, b: i64) -> i64 { a + b } // no self => static method
}

// in #[php_module]: module.function(...).class::<Calc>()
```

This was checked with a throwaway case (not committed): PHP printed `7` for
`\SpeedySpec\Proof\Calc::addTwo(3, 4)`, and
`ReflectionClass::getExtensionName()` reported `speedyspec-php`, the crate's
package name. Method names are converted to camelCase by default
(`add_two` -> `addTwo`); `#[php(name = "...")]` on a method overrides that.
The committed proof only exercises a function; Task 3 adds the first class.

Constraints of this mechanism:

- `php_module_startup` takes exactly one additional module, so everything
  native lives in one `#[php_module]` (in `speedyspec-php`).
- `#[php_module]` exports an unmangled `get_module` symbol; there can be only
  one in the binary.
- The engine starts once per process (`Php::boot` returns `AlreadyBooted` on
  a second call) and is not `Send`: the build is non-thread-safe.

**INI.** `php_ini_ignore = 1` stops PHP reading any php.ini, `PHPRC` and
`PHP_INI_SCAN_DIR`. Verified: with `PHPRC` and `PHP_INI_SCAN_DIR` pointing at
a directory holding a `php.ini`, the static `php` CLI loads it but the test
binary still sees `php_ini_loaded_file() === false`. The SAPI's fixed INI
entries mirror PHP's CLI SAPI (`display_errors=1`, `html_errors=0`,
`implicit_flush=1`, `output_buffering=0`, `max_execution_time=0`,
`log_errors=0`) except `register_argc_argv=0`: under any SAPI other than
`cli`, PHP 8.5 would derive `$_SERVER['argv']` from the query string and
emit a deprecation, so `Php::run_file` sets `$argv` and `$argc` itself
(`$_SERVER['argv']` is not populated yet).

**Running code.** `Php::eval(label, code)` calls `zend_eval_string` with a
null result pointer, so `code` is statements (no `<?php`), and `label` stands
in for the file name in errors. `Php::run_file(path, argv)` uses
`Embed::run_script` (`php_execute_script`) and returns `EG(exit_status)`.
Both run inside a zend bailout frame (ext-php-rs `try_catch_first` /
`try_catch`), so a PHP fatal error comes back as an error instead of a
`longjmp` through Rust frames.

Observed with throwaway cases (not committed), all in one booted engine:
`eval` of a `throw`, of invalid syntax, and of a call with too few arguments
returns `UncaughtException` with class `RuntimeException`, `ParseError` and
`ArgumentCountError`; `run_file` of a script doing `exit(3)` returns `Ok(3)`
and sees `$argv`/`$argc` as given; `run_file` of a script that throws lets
PHP print `Fatal error: Uncaught RuntimeException: ...` to stdout (as the PHP
CLI does with `display_errors=1`) and returns `Ok(255)`; and the engine still
runs code afterwards.

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

Linux x86_64 timings are under QEMU emulation on an arm64 host; a native
x86_64 CI runner should be faster.

## Verification

`cargo test -p speedyspec-php --test php` prints three `ok` lines and exits 0
on both targets.

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
- `register_argc_argv=0`; `run_file` sets `$argv`/`$argc` itself.

## CI

`.github/workflows/ci.yml` runs on `macos-latest` (arm64) and `ubuntu-latest`
(through `scripts/linux.sh`, so tests run in the same musl container used
locally), caches `.php-build*/spc` and `.php-build*/buildroot` keyed on
`php-version` and `scripts/build-php.sh`, builds PHP, and runs
`cargo test --workspace`. **It has never run**: the repository does not exist
on GitHub yet.
