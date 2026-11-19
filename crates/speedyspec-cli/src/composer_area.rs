//! Autoloading a plugin's composer packages, in Rust. A plugin whose `vendor/speedyspec-autoload.json` exists (written
//! by `speedyspec composer install`) gets its maps registered here before its entry file runs. One native function,
//! `SpeedySpec\Composer\Native\autoload`, is given to `spl_autoload_register` (appended, as composer's own autoloader
//! is when it does not prepend), looks a class up in every registered plugin's maps in load order, and requires the
//! file. The `files` entries are required when the plugin is registered. No PHP of composer's is involved.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use speedyspec_composer::{Lock, MAPS_FILE, Maps};
use speedyspec_php::NativeArea;
use speedyspec_php::ext_php_rs::builders::FunctionBuilder;
use speedyspec_php::native::{self, NativeError};
use speedyspec_php_ext::Value;

const AUTOLOAD: &str = "SpeedySpec\\Composer\\Native\\autoload";

#[derive(Default)]
struct State {
    /// Each registered plugin's directory and maps, in load order.
    plugins: Vec<(PathBuf, Maps)>,
    registered: bool,
    /// Package name to (version, plugin) of the first plugin that brought it.
    packages: BTreeMap<String, (String, String)>,
    /// The `files` already required. A package's file is keyed by its path under `vendor/`, so two plugins that
    /// bring the same package include it once, as composer does; a root file by its absolute path.
    files_done: BTreeSet<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// The `composer` area: the native autoloader.
pub struct ComposerArea;

impl NativeArea for ComposerArea {
    fn functions(&self) -> Vec<FunctionBuilder<'static>> {
        vec![native::function(AUTOLOAD, autoload)]
    }

    fn request_end(&self) {
        STATE.with(|s| *s.borrow_mut() = State::default());
    }
}

/// PHP's single-quoted form of `text`.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Requires `file`, as PHP's own `require` would.
fn require(file: &Path) -> Result<(), String> {
    let path = file.to_string_lossy();
    if path.contains('\0') {
        return Err(format!("{}: the path holds a NUL byte", file.display()));
    }
    speedyspec_php::eval(&path, &format!("require {};", quoted(&path)))
        .map(drop)
        .map_err(|e| e.to_string())
}

fn autoload(args: &[Value]) -> Result<Value, NativeError> {
    let Some(Value::Str(class)) = args.first() else {
        return Ok(Value::Null);
    };
    let found = STATE.with(|s| {
        s.borrow()
            .plugins
            .iter()
            .find_map(|(base, maps)| maps.find(base, class))
    });
    if let Some(file) = found {
        require(&file).map_err(|message| NativeError::throw("Error", message))?;
    }
    Ok(Value::Null)
}

/// Registers the plugin in `dir` (named `name`) for autoloading, if it has composer packages, and requires their
/// `files`. A plugin that locks packages but has not had them installed is refused with the command that fixes it.
/// Gives back warnings: a package another plugin already brought at a different version.
pub fn load(dir: &Path, name: &str) -> Result<Vec<String>, String> {
    // A plugin may bring its own autoloader (composer's vendor/, required by its entry file), so what speedyspec
    // cannot read or has not installed is a warning, never a reason to skip the plugin.
    let maps = match Maps::read(dir) {
        Ok(Some(maps)) => maps,
        Ok(None) => {
            let needs = match Lock::read(dir) {
                Ok(Some(lock)) => !lock.packages.is_empty(),
                Ok(None) => false,
                Err(_) => true,
            };
            return Ok(if needs {
                vec![format!(
                    "{name}'s composer packages are not installed by speedyspec ({MAPS_FILE} is missing); if it does \
                     not bring its own autoloader, run speedyspec composer install --working-dir {}",
                    dir.display()
                )]
            } else {
                Vec::new()
            });
        }
        Err(e) => {
            return Ok(vec![format!(
                "{name}: {e}; its composer packages are not autoloaded"
            )]);
        }
    };
    let lock = Lock::read(dir).ok().flatten();
    let mut warnings = Vec::new();
    // Packages another plugin brought first: theirs are used, so this plugin's copies add no files.
    let mut theirs: Vec<String> = Vec::new();
    let mut ours: Vec<String> = Vec::new();
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        for package in lock.iter().flat_map(|l| l.packages.iter()) {
            match state.packages.get(&package.name) {
                Some((version, first)) => {
                    if *version != package.version {
                        warnings.push(format!(
                            "{name} brings {} {}, but {first} already brought {version}, which is the one used",
                            package.name, package.version
                        ));
                    }
                    theirs.push(format!("vendor/{}/", package.name));
                }
                None => {
                    state
                        .packages
                        .insert(package.name.clone(), (package.version.clone(), name.to_owned()));
                    ours.push(package.name.clone());
                }
            }
        }
        state.plugins.push((dir.to_path_buf(), maps.clone()));
    });
    let loaded = (|| -> Result<(), String> {
        if !STATE.with(|s| s.borrow().registered) {
            speedyspec_php::call(
                &Value::Str("spl_autoload_register".to_owned()),
                &[Value::Str(AUTOLOAD.to_owned())],
            )
            .map_err(|e| format!("could not register the autoloader: {e}"))?;
            STATE.with(|s| s.borrow_mut().registered = true);
        }
        for file in &maps.files {
            if theirs
                .iter()
                .any(|package| file.starts_with(package.as_str()))
            {
                continue;
            }
            let key = if file.starts_with("vendor/") {
                file.clone()
            } else {
                dir.join(file).to_string_lossy().into_owned()
            };
            // A file stays done even if this plugin is skipped: what it declared is still declared.
            if STATE.with(|s| s.borrow_mut().files_done.insert(key)) {
                require(&dir.join(file))?;
            }
        }
        Ok(())
    })();
    if loaded.is_err() {
        // A plugin that is skipped autoloads nothing for the others either.
        STATE.with(|s| {
            let mut state = s.borrow_mut();
            state.plugins.retain(|(plugin, _)| plugin != dir);
            for package in &ours {
                state.packages.remove(package);
            }
        });
    }
    loaded.map(|()| warnings)
}
