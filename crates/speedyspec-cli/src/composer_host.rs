//! A Rust replica of the part of composer that hosts composer plugins: the interfaces plugins implement
//! (`PluginInterface`, `EventSubscriberInterface`, `Capable`), the objects composer hands them (`Composer`, `Config`,
//! the packages, the local repository, the installation manager, an `IOInterface`, the events), and the event-name
//! constants. Everything is declared natively from Rust; the only PHP that runs is the plugins' own code.
//!
//! Each object is a node in Rust: its methods read the node's data and never call back into PHP, so no PHP code runs
//! inside them and nothing can unwind through their frames. Plugins are activated and their listeners called from
//! the Rust host loop (`composer_child`), through `speedyspec_php::call`, which guards every entry into PHP.
//! A method this replica lacks fails with `composer's <Class>::<method> is not supported by speedyspec`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use serde_json::Value as Json;
use speedyspec_php::NativeArea;
use speedyspec_php::ext_php_rs::args::{Arg, ArgInfoTables};
use speedyspec_php::ext_php_rs::boxed::ZBox;
use speedyspec_php::ext_php_rs::builders::{ClassBuilder, FunctionBuilder};
use speedyspec_php::ext_php_rs::exception::PhpException;
use speedyspec_php::ext_php_rs::flags::{ClassFlags, ConstantFlags, DataType, MethodFlags};
use speedyspec_php::ext_php_rs::types::{ZendHashTable, Zval};
use speedyspec_php::ext_php_rs::zend::{ClassEntry, ExecuteData};

/// The plugin API version this replica answers to.
pub const PLUGIN_API_VERSION: &str = "2.6.0";

/// One object composer hands a plugin.
#[derive(Clone, Debug)]
pub enum Node {
    Composer,
    Config { vendor: PathBuf },
    Package(PackageData),
    RepositoryManager,
    Repository,
    InstallationManager { vendor: PathBuf },
    EventDispatcher,
    Io,
    ScriptEvent { name: String },
    PackageEvent { name: String, operation: usize },
    Operation { package: usize },
}

/// What a package object tells.
#[derive(Clone, Debug)]
pub struct PackageData {
    pub name: String,
    pub version: String,
    pub kind: String,
    pub extra: Json,
    pub autoload: Json,
    pub dist_url: Option<String>,
}

/// The graph the plugins see, and the PHP object of each node once it has been made.
#[derive(Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    objects: Vec<Option<Zval>>,
    by_handle: HashMap<u32, usize>,
    pub composer: usize,
    pub root: usize,
    pub config: usize,
    pub repository_manager: usize,
    pub repository: usize,
    pub installation_manager: usize,
    pub dispatcher: usize,
    pub io: usize,
    /// The installed packages, in lock order.
    pub packages: Vec<usize>,
}

impl Graph {
    pub fn add(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.objects.push(None);
        self.nodes.len() - 1
    }
}

thread_local! {
    static GRAPH: RefCell<Graph> = RefCell::new(Graph::default());
    /// Class entries by name, for parents and interfaces (filled as classes register).
    static CLASSES: RefCell<HashMap<String, usize>> = RefCell::new(HashMap::new());
}

/// Installs the graph the next objects are made from.
pub fn set_graph(graph: Graph) {
    GRAPH.with(|g| *g.borrow_mut() = graph);
}

/// Drops every object reference the graph holds; called while the request is still open.
pub fn clear_graph() {
    GRAPH.with(|g| *g.borrow_mut() = Graph::default());
}

/// The PHP class of a node.
pub fn class_of(node: &Node, root: bool) -> &'static str {
    match node {
        Node::Composer => "Composer\\Composer",
        Node::Config { .. } => "Composer\\Config",
        Node::Package(_) if root => "Composer\\Package\\RootPackage",
        Node::Package(_) => "Composer\\Package\\Package",
        Node::RepositoryManager => "Composer\\Repository\\RepositoryManager",
        Node::Repository => "Composer\\Repository\\InstalledArrayRepository",
        Node::InstallationManager { .. } => "Composer\\Installer\\InstallationManager",
        Node::EventDispatcher => "Composer\\EventDispatcher\\EventDispatcher",
        Node::Io => "Composer\\IO\\ConsoleIO",
        Node::ScriptEvent { .. } => "Composer\\Script\\Event",
        Node::PackageEvent { .. } => "Composer\\Installer\\PackageEvent",
        Node::Operation { .. } => "Composer\\DependencyResolver\\Operation\\InstallOperation",
    }
}

// ----- class declarations

enum Constant {
    Str(&'static str, &'static str),
    Int(&'static str, i64),
}

struct Spec {
    name: &'static str,
    interface: bool,
    parent: Option<&'static str>,
    implements: &'static [&'static str],
    /// Methods answered by `dispatch`; classes with nodes also get `__construct` and `__call`.
    methods: &'static [&'static str],
    constants: &'static [Constant],
    nodes: bool,
}

const fn interface(name: &'static str, implements: &'static [&'static str]) -> Spec {
    Spec {
        name,
        interface: true,
        parent: None,
        implements,
        methods: &[],
        constants: &[],
        nodes: false,
    }
}

const fn constants(name: &'static str, constants: &'static [Constant]) -> Spec {
    Spec {
        name,
        interface: false,
        parent: None,
        implements: &[],
        methods: &[],
        constants,
        nodes: false,
    }
}

const fn class(
    name: &'static str,
    parent: Option<&'static str>,
    implements: &'static [&'static str],
    methods: &'static [&'static str],
) -> Spec {
    Spec {
        name,
        interface: false,
        parent,
        implements,
        methods,
        constants: &[],
        nodes: true,
    }
}

const PACKAGE_METHODS: &[&str] = &[
    "getName",
    "getPrettyName",
    "getVersion",
    "getPrettyVersion",
    "getType",
    "getExtra",
    "getAutoload",
    "getTargetDir",
    "getInstallationSource",
    "getDistType",
    "getDistUrl",
    "getUniqueName",
];

const IO_METHODS: &[&str] = &[
    "write",
    "writeError",
    "writeRaw",
    "writeErrorRaw",
    "overwrite",
    "overwriteError",
    "isInteractive",
    "isVerbose",
    "isVeryVerbose",
    "isDebug",
    "isDecorated",
    "ask",
    "askConfirmation",
    "askAndValidate",
    "askAndHideAnswer",
    "select",
    "getAuthentications",
    "hasAuthentication",
    "getAuthentication",
    "setAuthentication",
    "loadConfiguration",
    "emergency",
    "alert",
    "critical",
    "error",
    "warning",
    "notice",
    "info",
    "debug",
    "log",
];

/// Every class, in an order where parents and interfaces come first.
const SPECS: &[Spec] = &[
    Spec {
        constants: &[Constant::Str("PLUGIN_API_VERSION", PLUGIN_API_VERSION)],
        ..interface("Composer\\Plugin\\PluginInterface", &[])
    },
    interface("Composer\\Plugin\\Capable", &[]),
    interface("Composer\\Plugin\\Capability\\Capability", &[]),
    interface("Composer\\EventDispatcher\\EventSubscriberInterface", &[]),
    Spec {
        constants: &[
            Constant::Int("QUIET", 1),
            Constant::Int("NORMAL", 2),
            Constant::Int("VERBOSE", 4),
            Constant::Int("VERY_VERBOSE", 8),
            Constant::Int("DEBUG", 16),
        ],
        ..interface("Composer\\IO\\IOInterface", &[])
    },
    interface("Composer\\Package\\PackageInterface", &[]),
    interface(
        "Composer\\Package\\CompletePackageInterface",
        &["Composer\\Package\\PackageInterface"],
    ),
    interface(
        "Composer\\Package\\RootPackageInterface",
        &["Composer\\Package\\CompletePackageInterface"],
    ),
    interface("Composer\\Repository\\RepositoryInterface", &[]),
    interface(
        "Composer\\Repository\\InstalledRepositoryInterface",
        &["Composer\\Repository\\RepositoryInterface"],
    ),
    interface(
        "Composer\\DependencyResolver\\Operation\\OperationInterface",
        &[],
    ),
    constants(
        "Composer\\Script\\ScriptEvents",
        &[
            Constant::Str("PRE_INSTALL_CMD", "pre-install-cmd"),
            Constant::Str("POST_INSTALL_CMD", "post-install-cmd"),
            Constant::Str("PRE_UPDATE_CMD", "pre-update-cmd"),
            Constant::Str("POST_UPDATE_CMD", "post-update-cmd"),
            Constant::Str("PRE_STATUS_CMD", "pre-status-cmd"),
            Constant::Str("POST_STATUS_CMD", "post-status-cmd"),
            Constant::Str("PRE_ARCHIVE_CMD", "pre-archive-cmd"),
            Constant::Str("POST_ARCHIVE_CMD", "post-archive-cmd"),
            Constant::Str("PRE_AUTOLOAD_DUMP", "pre-autoload-dump"),
            Constant::Str("POST_AUTOLOAD_DUMP", "post-autoload-dump"),
            Constant::Str("POST_ROOT_PACKAGE_INSTALL", "post-root-package-install"),
            Constant::Str("POST_CREATE_PROJECT_CMD", "post-create-project-cmd"),
        ],
    ),
    constants(
        "Composer\\Installer\\PackageEvents",
        &[
            Constant::Str("PRE_PACKAGE_INSTALL", "pre-package-install"),
            Constant::Str("POST_PACKAGE_INSTALL", "post-package-install"),
            Constant::Str("PRE_PACKAGE_UPDATE", "pre-package-update"),
            Constant::Str("POST_PACKAGE_UPDATE", "post-package-update"),
            Constant::Str("PRE_PACKAGE_UNINSTALL", "pre-package-uninstall"),
            Constant::Str("POST_PACKAGE_UNINSTALL", "post-package-uninstall"),
        ],
    ),
    constants(
        "Composer\\Plugin\\PluginEvents",
        &[
            Constant::Str("INIT", "init"),
            Constant::Str("COMMAND", "command"),
            Constant::Str("PRE_FILE_DOWNLOAD", "pre-file-download"),
            Constant::Str("POST_FILE_DOWNLOAD", "post-file-download"),
            Constant::Str("PRE_COMMAND_RUN", "pre-command-run"),
            Constant::Str("PRE_POOL_CREATE", "pre-pool-create"),
        ],
    ),
    class(
        "Composer\\Composer",
        None,
        &[],
        &[
            "getPackage",
            "getConfig",
            "getRepositoryManager",
            "getInstallationManager",
            "getEventDispatcher",
        ],
    ),
    class("Composer\\Config", None, &[], &["get", "all"]),
    class(
        "Composer\\Package\\Package",
        None,
        &["Composer\\Package\\CompletePackageInterface"],
        PACKAGE_METHODS,
    ),
    class(
        "Composer\\Package\\RootPackage",
        Some("Composer\\Package\\Package"),
        &["Composer\\Package\\RootPackageInterface"],
        &[],
    ),
    class(
        "Composer\\Repository\\RepositoryManager",
        None,
        &[],
        &["getLocalRepository"],
    ),
    class(
        "Composer\\Repository\\InstalledArrayRepository",
        None,
        &["Composer\\Repository\\InstalledRepositoryInterface"],
        &[
            "getPackages",
            "getCanonicalPackages",
            "findPackage",
            "hasPackage",
            "count",
        ],
    ),
    class(
        "Composer\\Installer\\InstallationManager",
        None,
        &[],
        &["getInstallPath"],
    ),
    class("Composer\\EventDispatcher\\EventDispatcher", None, &[], &[]),
    class(
        "Composer\\IO\\ConsoleIO",
        None,
        &["Composer\\IO\\IOInterface"],
        IO_METHODS,
    ),
    class(
        "Composer\\EventDispatcher\\Event",
        None,
        &[],
        &[
            "getName",
            "getArguments",
            "getFlags",
            "isPropagationStopped",
            "stopPropagation",
        ],
    ),
    class(
        "Composer\\Script\\Event",
        Some("Composer\\EventDispatcher\\Event"),
        &[],
        &["getComposer", "getIO", "isDevMode", "getOriginatingEvent"],
    ),
    class(
        "Composer\\Installer\\PackageEvent",
        Some("Composer\\EventDispatcher\\Event"),
        &[],
        &[
            "getComposer",
            "getIO",
            "isDevMode",
            "getOperation",
            "getOperations",
            "getLocalRepo",
        ],
    ),
    class(
        "Composer\\DependencyResolver\\Operation\\InstallOperation",
        None,
        &["Composer\\DependencyResolver\\Operation\\OperationInterface"],
        &["getPackage", "getOperationType", "show"],
    ),
];

fn remember(class: &'static mut ClassEntry, tables: ArgInfoTables) {
    // The engine keeps pointing into the argument tables for the life of the process.
    std::mem::forget(tables);
    let name = class.name().unwrap_or_default().to_owned();
    let entry = std::ptr::from_mut(class) as usize;
    CLASSES.with(|c| c.borrow_mut().insert(name, entry));
}

fn entry(name: &str) -> &'static ClassEntry {
    let pointer = CLASSES.with(|c| c.borrow().get(name).copied());
    let pointer =
        pointer.unwrap_or_else(|| panic!("{name} is declared after a class that needs it"));
    // SAFETY: the engine owns registered class entries for the life of the process.
    unsafe { &*(pointer as *const ClassEntry) }
}

/// `ClassEntryInfo` needs a plain function per class it names.
macro_rules! entry_fn {
    ($($f:ident => $name:literal),* $(,)?) => {
        $(fn $f() -> &'static ClassEntry { entry($name) })*
        fn entry_fn(name: &str) -> fn() -> &'static ClassEntry {
            match name {
                $($name => $f,)*
                other => panic!("no entry function for {other}"),
            }
        }
    };
}

entry_fn! {
    ce_complete_package => "Composer\\Package\\CompletePackageInterface",
    ce_package_interface => "Composer\\Package\\PackageInterface",
    ce_root_package_interface => "Composer\\Package\\RootPackageInterface",
    ce_repository_interface => "Composer\\Repository\\RepositoryInterface",
    ce_installed_repository => "Composer\\Repository\\InstalledRepositoryInterface",
    ce_operation_interface => "Composer\\DependencyResolver\\Operation\\OperationInterface",
    ce_io_interface => "Composer\\IO\\IOInterface",
    ce_package => "Composer\\Package\\Package",
    ce_event => "Composer\\EventDispatcher\\Event",
}

fn method(name: &str) -> FunctionBuilder<'static> {
    FunctionBuilder::new(name, dispatch).arg(Arg::new("args", DataType::Mixed).is_variadic())
}

/// The area that declares the replica's classes; booted only in the composer-plugin process.
pub struct ComposerHostArea;

impl NativeArea for ComposerHostArea {
    fn register_classes(&self) -> Result<(), String> {
        for spec in SPECS {
            let mut builder = ClassBuilder::new(spec.name).registration(remember);
            if spec.interface {
                builder = builder.flags(ClassFlags::Interface);
            }
            if let Some(parent) = spec.parent {
                builder = builder.extends((entry_fn(parent), parent));
            }
            for &implemented in spec.implements {
                builder = builder.implements((entry_fn(implemented), implemented));
            }
            for constant in spec.constants {
                builder = match constant {
                    Constant::Str(name, value) => {
                        builder.constant(*name, *value, &[], ConstantFlags::Public)
                    }
                    Constant::Int(name, value) => {
                        builder.constant(*name, *value, &[], ConstantFlags::Public)
                    }
                }
                .map_err(|e| format!("{}: {e}", spec.name))?;
            }
            if spec.nodes {
                builder = builder
                    .method(
                        FunctionBuilder::new("__construct", dispatch)
                            .arg(Arg::new("id", DataType::Long)),
                        MethodFlags::Public,
                    )
                    .method(
                        FunctionBuilder::new("__call", dispatch)
                            .arg(Arg::new("name", DataType::String))
                            .arg(Arg::new("arguments", DataType::Array)),
                        MethodFlags::Public,
                    );
            }
            for &name in spec.methods {
                builder = builder.method(method(name), MethodFlags::Public);
            }
            builder
                .register()
                .map_err(|e| format!("{}: {e}", spec.name))?;
        }
        Ok(())
    }

    fn request_end(&self) {
        clear_graph();
    }
}

// ----- method dispatch

/// What a method gives back, before it becomes a zval.
enum Answer {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Json(Json),
    Object(usize),
    Objects(Vec<usize>),
}

fn zend_str(raw: *mut speedyspec_php::ext_php_rs::ffi::zend_string) -> String {
    if raw.is_null() {
        return String::new();
    }
    // SAFETY: a live zend_string from the engine: `len` bytes from `val`.
    unsafe {
        let bytes = std::slice::from_raw_parts((*raw).val.as_ptr().cast::<u8>(), (*raw).len);
        String::from_utf8_lossy(bytes).into_owned()
    }
}

extern "C" fn dispatch(ex: &mut ExecuteData, retval: &mut Zval) {
    let outcome = catch_unwind(AssertUnwindSafe(|| handle(ex, retval)));
    let failure = match outcome {
        Ok(Ok(())) => return,
        Ok(Err(message)) => message,
        Err(_) => "speedyspec's composer failed inside a method".to_owned(),
    };
    retval.set_null();
    PhpException::from_message(failure).throw();
}

fn args(ex: &ExecuteData) -> Vec<Zval> {
    // SAFETY: all members of `This.u2` are the same u32, the frame's argument count.
    let count = unsafe { ex.This.u2.num_args } as usize;
    (0..count)
        // SAFETY: `i < num_args`, so the slot is inside the live frame.
        .filter_map(|i| unsafe { ex.zend_call_arg(i) }.map(|zv| zv.shallow_clone()))
        .collect()
}

fn short(class: &str) -> &str {
    class.rsplit('\\').next().unwrap_or(class)
}

fn handle(ex: &mut ExecuteData, retval: &mut Zval) -> Result<(), String> {
    // SAFETY: PHP called us with a live frame whose `func` is this method.
    let method = unsafe { zend_str((*ex.func).common.function_name) };
    let args = args(ex);
    let this = ex
        .get_self()
        .ok_or_else(|| format!("composer's {method} must be called on an object"))?;
    let class = this.get_class_name().unwrap_or_default();
    let handle = this.handle;
    if method == "__construct" {
        let id = args
            .first()
            .and_then(Zval::long)
            .and_then(|id| usize::try_from(id).ok())
            .ok_or("speedyspec's composer objects are made by speedyspec")?;
        return GRAPH.with(|g| {
            let mut g = g.borrow_mut();
            let slot = g
                .objects
                .get_mut(id)
                .ok_or("speedyspec's composer objects are made by speedyspec")?;
            let mut held = Zval::new();
            held.set_object(this);
            *slot = Some(held);
            g.by_handle.insert(handle, id);
            Ok(())
        });
    }
    if method == "__call" {
        let called = args.first().and_then(Zval::string).unwrap_or_default();
        return Err(format!(
            "composer's {}::{called} is not supported by speedyspec",
            short(&class)
        ));
    }
    let answer = GRAPH.with(|g| {
        let g = g.borrow();
        let id = *g
            .by_handle
            .get(&handle)
            .ok_or("this composer object was not made by speedyspec")?;
        answer(&g, id, &method, &args).ok_or_else(|| {
            format!(
                "composer's {}::{method} is not supported by speedyspec",
                short(&class)
            )
        })
    })?;
    fill(retval, answer)
}

fn package_of(g: &Graph, id: usize) -> Option<&PackageData> {
    match g.nodes.get(id)? {
        Node::Package(p) => Some(p),
        _ => None,
    }
}

/// The id of the package object passed as `arg`.
fn package_arg(g: &Graph, arg: Option<&Zval>) -> Option<usize> {
    let object = arg?.object()?;
    let id = *g.by_handle.get(&object.handle)?;
    package_of(g, id).map(|_| id)
}

fn messages(args: &[Zval]) -> Vec<String> {
    match args.first() {
        Some(zv) => match zv.array() {
            Some(list) => list.values().filter_map(Zval::string).collect(),
            None => zv.string().into_iter().collect(),
        },
        None => Vec::new(),
    }
}

/// What `method` on node `id` answers; `None` when this replica does not have it.
fn answer(g: &Graph, id: usize, method: &str, args: &[Zval]) -> Option<Answer> {
    let node = g.nodes.get(id)?;
    Some(match (node, method) {
        (Node::Composer, "getPackage") => Answer::Object(g.root),
        (Node::Composer, "getConfig") => Answer::Object(g.config),
        (Node::Composer, "getRepositoryManager") => Answer::Object(g.repository_manager),
        (Node::Composer, "getInstallationManager") => Answer::Object(g.installation_manager),
        (Node::Composer, "getEventDispatcher") => Answer::Object(g.dispatcher),
        (Node::Config { vendor }, "get") => {
            let key = args.first().and_then(Zval::string).unwrap_or_default();
            match key.as_str() {
                "vendor-dir" => Answer::Str(vendor.to_string_lossy().into_owned()),
                "bin-dir" => Answer::Str(vendor.join("bin").to_string_lossy().into_owned()),
                "process-timeout" => Answer::Int(300),
                _ => Answer::Null,
            }
        }
        (Node::Config { vendor }, "all") => Answer::Json(serde_json::json!({
            "config": {"vendor-dir": vendor.to_string_lossy(), "bin-dir": vendor.join("bin").to_string_lossy()}
        })),
        (Node::Package(p), "getName" | "getPrettyName") => Answer::Str(p.name.clone()),
        (Node::Package(p), "getVersion" | "getPrettyVersion") => Answer::Str(p.version.clone()),
        (Node::Package(p), "getType") => Answer::Str(p.kind.clone()),
        (Node::Package(p), "getExtra") => Answer::Json(p.extra.clone()),
        (Node::Package(p), "getAutoload") => Answer::Json(p.autoload.clone()),
        (Node::Package(_), "getTargetDir") => Answer::Null,
        (Node::Package(p), "getInstallationSource") => {
            if p.dist_url.is_some() {
                Answer::Str("dist".into())
            } else {
                Answer::Null
            }
        }
        (Node::Package(p), "getDistType") => {
            if p.dist_url.is_some() {
                Answer::Str("zip".into())
            } else {
                Answer::Null
            }
        }
        (Node::Package(p), "getDistUrl") => p.dist_url.clone().map_or(Answer::Null, Answer::Str),
        (Node::Package(p), "getUniqueName") => Answer::Str(format!("{}-{}", p.name, p.version)),
        (Node::RepositoryManager, "getLocalRepository") => Answer::Object(g.repository),
        (Node::Repository, "getPackages" | "getCanonicalPackages") => {
            Answer::Objects(g.packages.clone())
        }
        (Node::Repository, "count") => Answer::Int(g.packages.len() as i64),
        (Node::Repository, "findPackage") => {
            let name = args
                .first()
                .and_then(Zval::string)
                .unwrap_or_default()
                .to_lowercase();
            g.packages
                .iter()
                .find(|&&p| package_of(g, p).is_some_and(|d| d.name == name))
                .map_or(Answer::Null, |&p| Answer::Object(p))
        }
        (Node::Repository, "hasPackage") => {
            let asked = package_arg(g, args.first());
            Answer::Bool(asked.is_some_and(|a| g.packages.contains(&a)))
        }
        (Node::InstallationManager { vendor }, "getInstallPath") => {
            let package = package_arg(g, args.first()).and_then(|p| package_of(g, p))?;
            Answer::Str(vendor.join(&package.name).to_string_lossy().into_owned())
        }
        (
            Node::Io,
            "write" | "writeError" | "writeRaw" | "writeErrorRaw" | "overwrite" | "overwriteError",
        ) => {
            for line in messages(args) {
                eprintln!("{line}");
            }
            Answer::Null
        }
        (
            Node::Io,
            "emergency" | "alert" | "critical" | "error" | "warning" | "notice" | "info" | "debug",
        ) => {
            for line in messages(args) {
                eprintln!("{method}: {line}");
            }
            Answer::Null
        }
        (Node::Io, "log") => {
            let level = args.first().and_then(Zval::string).unwrap_or_default();
            let line = args.get(1).and_then(Zval::string).unwrap_or_default();
            eprintln!("{level}: {line}");
            Answer::Null
        }
        (Node::Io, "isInteractive" | "isVerbose" | "isVeryVerbose" | "isDebug" | "isDecorated") => {
            Answer::Bool(false)
        }
        // Never interactive: every question takes its default.
        (Node::Io, "ask" | "askAndValidate") => default_answer(args.get(1)),
        (Node::Io, "askConfirmation") => args.get(1).map_or(Answer::Bool(true), |zv| {
            Answer::Bool(zv.bool().unwrap_or(true))
        }),
        (Node::Io, "select") => default_answer(args.get(2)),
        (Node::Io, "askAndHideAnswer") => Answer::Null,
        (Node::Io, "getAuthentications") => Answer::Json(serde_json::json!({})),
        (Node::Io, "hasAuthentication") => Answer::Bool(false),
        (Node::Io, "getAuthentication") => {
            Answer::Json(serde_json::json!({"username": null, "password": null}))
        }
        (Node::Io, "setAuthentication" | "loadConfiguration") => Answer::Null,
        (Node::ScriptEvent { name } | Node::PackageEvent { name, .. }, "getName") => {
            Answer::Str(name.clone())
        }
        (Node::ScriptEvent { .. } | Node::PackageEvent { .. }, "getArguments" | "getFlags") => {
            Answer::Json(serde_json::json!([]))
        }
        (
            Node::ScriptEvent { .. } | Node::PackageEvent { .. },
            "isPropagationStopped" | "isDevMode",
        ) => Answer::Bool(false),
        (Node::ScriptEvent { .. } | Node::PackageEvent { .. }, "stopPropagation") => Answer::Null,
        (Node::ScriptEvent { .. } | Node::PackageEvent { .. }, "getComposer") => {
            Answer::Object(g.composer)
        }
        (Node::ScriptEvent { .. } | Node::PackageEvent { .. }, "getIO") => Answer::Object(g.io),
        (Node::ScriptEvent { .. }, "getOriginatingEvent") => Answer::Null,
        (Node::PackageEvent { operation, .. }, "getOperation") => Answer::Object(*operation),
        (Node::PackageEvent { operation, .. }, "getOperations") => {
            Answer::Objects(vec![*operation])
        }
        (Node::PackageEvent { .. }, "getLocalRepo") => Answer::Object(g.repository),
        (Node::Operation { package }, "getPackage") => Answer::Object(*package),
        (Node::Operation { .. }, "getOperationType") => Answer::Str("install".into()),
        (Node::Operation { package }, "show") => {
            let p = package_of(g, *package)?;
            Answer::Str(format!("Installing {} ({})", p.name, p.version))
        }
        _ => return None,
    })
}

fn default_answer(arg: Option<&Zval>) -> Answer {
    match arg {
        Some(zv) => {
            if let Some(text) = zv.string() {
                Answer::Str(text)
            } else if let Some(n) = zv.long() {
                Answer::Int(n)
            } else if let Some(b) = zv.bool() {
                Answer::Bool(b)
            } else {
                Answer::Null
            }
        }
        None => Answer::Null,
    }
}

fn json_zval(value: &Json) -> Result<Zval, String> {
    let mut zv = Zval::new();
    match value {
        Json::Null => zv.set_null(),
        Json::Bool(b) => zv.set_bool(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => zv.set_long(i),
            None => zv.set_double(n.as_f64().unwrap_or_default()),
        },
        Json::String(s) => zv.set_string(s, false).map_err(|e| e.to_string())?,
        Json::Array(items) => {
            let mut table = ZendHashTable::new();
            for item in items {
                table.push(json_zval(item)?).map_err(|e| e.to_string())?;
            }
            zv.set_hashtable(table);
        }
        Json::Object(map) => {
            let mut table = ZendHashTable::new();
            for (key, item) in map {
                table
                    .insert(key.as_str(), json_zval(item)?)
                    .map_err(|e| e.to_string())?;
            }
            zv.set_hashtable(table);
        }
    }
    Ok(zv)
}

/// The object of node `id`, as a new reference.
fn object_zval(id: usize) -> Result<Zval, String> {
    GRAPH.with(|g| {
        let mut g = g.borrow_mut();
        let held = g
            .objects
            .get_mut(id)
            .and_then(Option::as_mut)
            .and_then(Zval::object_mut)
            .ok_or("speedyspec's composer has not made that object")?;
        let mut zv = Zval::new();
        zv.set_object(held);
        Ok(zv)
    })
}

fn fill(retval: &mut Zval, answer: Answer) -> Result<(), String> {
    match answer {
        Answer::Null => retval.set_null(),
        Answer::Bool(b) => retval.set_bool(b),
        Answer::Int(i) => retval.set_long(i),
        Answer::Str(s) => retval.set_string(&s, false).map_err(|e| e.to_string())?,
        Answer::Json(json) => *retval = json_zval(&json)?,
        Answer::Object(id) => *retval = object_zval(id)?,
        Answer::Objects(ids) => {
            let mut table: ZBox<ZendHashTable> = ZendHashTable::new();
            for id in ids {
                table.push(object_zval(id)?).map_err(|e| e.to_string())?;
            }
            retval.set_hashtable(table);
        }
    }
    Ok(())
}
