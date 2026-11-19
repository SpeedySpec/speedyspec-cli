//! Running composer plugins: the CLI starts itself again as `self composer-plugins --dir <dir>` (hidden), in a
//! process group of its own, so a plugin's state, its fatal errors and its `exit()` never reach the CLI. The child
//! boots PHP with the Rust autoloader and the Rust replica of composer's plugin API (`composer_host`), activates every
//! `composer-plugin` package in lock order, and dispatches what composer dispatches after an install:
//! `post-package-install` for each other package, then `post-autoload-dump` and `post-install-cmd`. Root scripts that
//! are PHP callables (`Class::method`) run with their event; other scripts are reported and not run.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::Value as Json;
use speedyspec_composer::Lock;
use speedyspec_hooks::{Key, Value};
use speedyspec_hooks_memory::MemoryHooks;
use speedyspec_php::{BootOptions, Php};

use crate::composer_host::{
    ComposerHostArea, Graph, Node, PackageData, class_of, clear_graph, set_graph,
};

/// How long the composer plugins of one install may take.
const TIMEOUT: Duration = Duration::from_secs(600);
const SUBSCRIBER: &str = "Composer\\EventDispatcher\\EventSubscriberInterface";

/// Runs the composer plugins and PHP-callable scripts of the packages installed in `dir`, in a child process.
pub fn run_plugins(dir: &Path) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot find the speedyspec executable: {e}"))?;
    let mut child = Command::new(exe)
        .args(["self", "composer-plugins", "--dir"])
        .arg(dir)
        .stdin(Stdio::null())
        // What a plugin prints is not this command's output.
        .stdout(std::io::stderr())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("could not start the composer plugins: {e}"))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= TIMEOUT => {
                // SAFETY: a negative pid names the group the child leads.
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
                let _ = child.wait();
                return Err(format!(
                    "the composer plugins did not finish within {}s and were stopped",
                    TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("could not wait for the composer plugins: {e}")),
        }
    };
    // Whatever a plugin started in the background goes with it.
    // SAFETY: as above.
    unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
    if status.success() {
        Ok(())
    } else {
        Err("the composer plugins failed".to_owned())
    }
}

/// `self composer-plugins --dir <dir>`: the child side.
pub fn run_child(dir: &Path) -> i32 {
    match host(dir) {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("speedyspec: {message}");
            1
        }
    }
}

/// One listener: who registered it, its priority and the callable.
struct Listener {
    owner: String,
    event: String,
    priority: i64,
    callable: Value,
}

fn is_class_name(name: &str) -> bool {
    let name = name.trim_start_matches('\\');
    !name.is_empty()
        && name.split('\\').all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || !c.is_ascii())
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii())
        })
}

fn pair(a: Value, b: &str) -> Value {
    Value::Array(vec![
        (Key::Int(0), a),
        (Key::Int(1), Value::Str(b.to_owned())),
    ])
}

/// The listeners `getSubscribedEvents()` gave: `event => 'method'`, `event => ['method', priority]` or
/// `event => [['method', priority], …]`.
fn subscriptions(
    owner: &str,
    plugin: &Value,
    events: &Value,
    into: &mut Vec<Listener>,
) -> Result<(), String> {
    let Value::Array(events) = events else {
        return Err(format!(
            "{owner}: getSubscribedEvents() did not return an array"
        ));
    };
    for (key, spec) in events {
        let Key::Str(event) = key else {
            continue;
        };
        let mut add = |method: &str, priority: i64| {
            into.push(Listener {
                owner: owner.to_owned(),
                event: event.clone(),
                priority,
                callable: pair(plugin.clone(), method),
            });
        };
        match spec {
            Value::Str(method) => add(method, 0),
            Value::Array(items) => {
                let first = items.first().map(|(_, v)| v);
                if let Some(Value::Str(method)) = first {
                    let priority = match items.get(1).map(|(_, v)| v) {
                        Some(Value::Int(p)) => *p,
                        _ => 0,
                    };
                    add(method, priority);
                } else {
                    for (_, item) in items {
                        let Value::Array(parts) = item else { continue };
                        let Some(Value::Str(method)) = parts.first().map(|(_, v)| v) else {
                            continue;
                        };
                        let priority = match parts.get(1).map(|(_, v)| v) {
                            Some(Value::Int(p)) => *p,
                            _ => 0,
                        };
                        add(method, priority);
                    }
                }
            }
            _ => {
                return Err(format!(
                    "{owner}: getSubscribedEvents() has a listener for {event} that is not a method name"
                ));
            }
        }
    }
    Ok(())
}

fn dispatch(php: &Php, listeners: &[Listener], event: &str, object: &Value) -> Result<(), String> {
    let mut chosen: Vec<&Listener> = listeners.iter().filter(|l| l.event == event).collect();
    // Higher priority first; registration order breaks ties (the sort is stable).
    chosen.sort_by_key(|l| std::cmp::Reverse(l.priority));
    for listener in chosen {
        php.call(&listener.callable, std::slice::from_ref(object))
            .map_err(|e| format!("{}: {event}: {e}", listener.owner))?;
    }
    Ok(())
}

fn host(dir: &Path) -> Result<(), String> {
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let lock = Lock::read(&dir)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("there is no composer.json in {}", dir.display()))?;
    let root_json: Json = std::fs::read_to_string(dir.join("composer.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Json::Null);
    let php = Php::boot(BootOptions {
        hooks: Rc::new(MemoryHooks::new()),
        preload: &[],
        areas: vec![
            Rc::new(crate::composer_area::ComposerArea),
            Rc::new(ComposerHostArea),
        ],
    })
    .map_err(|e| format!("PHP did not start: {e}"))?;
    for warning in crate::composer_area::load(&dir, "composer")? {
        eprintln!("speedyspec: warning: {warning}");
    }

    let vendor = dir.join("vendor");
    let mut graph = Graph::default();
    graph.composer = graph.add(Node::Composer);
    graph.root = graph.add(Node::Package(PackageData {
        name: root_json["name"].as_str().unwrap_or("__root__").to_owned(),
        version: root_json["version"]
            .as_str()
            .unwrap_or("1.0.0+no-version-set")
            .to_owned(),
        kind: root_json["type"].as_str().unwrap_or("project").to_owned(),
        extra: root_json
            .get("extra")
            .cloned()
            .unwrap_or(Json::Object(Default::default())),
        autoload: root_json
            .get("autoload")
            .cloned()
            .unwrap_or(Json::Object(Default::default())),
        dist_url: None,
    }));
    graph.config = graph.add(Node::Config {
        vendor: vendor.clone(),
    });
    graph.repository_manager = graph.add(Node::RepositoryManager);
    graph.repository = graph.add(Node::Repository);
    graph.installation_manager = graph.add(Node::InstallationManager {
        vendor: vendor.clone(),
    });
    graph.dispatcher = graph.add(Node::EventDispatcher);
    graph.io = graph.add(Node::Io);
    let mut package_events = Vec::new();
    for package in &lock.packages {
        let id = graph.add(Node::Package(PackageData {
            name: package.name.clone(),
            version: package.version.clone(),
            kind: package.kind.clone(),
            extra: if package.extra.is_null() {
                Json::Object(Default::default())
            } else {
                package.extra.clone()
            },
            autoload: Json::Object(Default::default()),
            dist_url: Some(package.dist.url.clone()),
        }));
        graph.packages.push(id);
        if package.kind != "composer-plugin" {
            let operation = graph.add(Node::Operation { package: id });
            let event = graph.add(Node::PackageEvent {
                name: "post-package-install".to_owned(),
                operation,
            });
            package_events.push(event);
        }
    }
    let dump = graph.add(Node::ScriptEvent {
        name: "post-autoload-dump".to_owned(),
    });
    let installed = graph.add(Node::ScriptEvent {
        name: "post-install-cmd".to_owned(),
    });
    let (composer_id, io_id) = (graph.composer, graph.io);
    let classes: Vec<&'static str> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(id, node)| class_of(node, id == graph.root))
        .collect();
    set_graph(graph);

    let result = (|| {
        let mut objects = Vec::with_capacity(classes.len());
        for (id, class) in classes.iter().enumerate() {
            let object = php
                .eval(
                    "speedyspec://composer",
                    &format!("return new \\{class}({id});"),
                )
                .map_err(|e| format!("could not make composer's {class}: {e}"))?;
            objects.push(object);
        }
        let object = |id: usize| objects[id].clone();
        let (composer, io) = (object(composer_id), object(io_id));

        let mut listeners = Vec::new();
        for package in lock.packages.iter().filter(|p| p.kind == "composer-plugin") {
            let owner = format!("{} {}", package.name, package.version);
            let named = match &package.extra["class"] {
                Json::String(one) => vec![one.clone()],
                Json::Array(many) => many
                    .iter()
                    .filter_map(|c| c.as_str().map(str::to_owned))
                    .collect(),
                _ => {
                    return Err(format!(
                        "{owner}: its extra.class does not name the plugin class"
                    ));
                }
            };
            for class in named {
                if !is_class_name(&class) {
                    return Err(format!("{owner}: {class:?} is not a class name"));
                }
                let plugin = php
                    .eval(
                        "speedyspec://composer",
                        &format!("return new \\{};", class.trim_start_matches('\\')),
                    )
                    .map_err(|e| format!("{owner}: {e}"))?;
                php.call(
                    &pair(plugin.clone(), "activate"),
                    &[composer.clone(), io.clone()],
                )
                .map_err(|e| format!("{owner}: activate: {e}"))?;
                let subscribes = php
                    .call(
                        &Value::Str("is_subclass_of".to_owned()),
                        &[Value::Str(class.clone()), Value::Str(SUBSCRIBER.to_owned())],
                    )
                    .map_err(|e| format!("{owner}: {e}"))?;
                if subscribes == Value::Bool(true) {
                    let events = php
                        .call(&Value::Str(format!("{class}::getSubscribedEvents")), &[])
                        .map_err(|e| format!("{owner}: getSubscribedEvents: {e}"))?;
                    subscriptions(&owner, &plugin, &events, &mut listeners)?;
                }
            }
        }
        for (event, commands) in &lock.scripts {
            for command in commands {
                let callable = command
                    .split_once("::")
                    .is_some_and(|(class, method)| is_class_name(class) && is_class_name(method));
                if callable {
                    listeners.push(Listener {
                        owner: "composer.json".to_owned(),
                        event: event.clone(),
                        priority: 0,
                        callable: Value::Str(command.trim_start_matches('\\').to_owned()),
                    });
                } else {
                    eprintln!(
                        "speedyspec: {event}: not run, speedyspec runs only PHP callables: {command}"
                    );
                }
            }
        }

        for event in package_events {
            dispatch(&php, &listeners, "post-package-install", &object(event))?;
        }
        dispatch(&php, &listeners, "post-autoload-dump", &object(dump))?;
        dispatch(&php, &listeners, "post-install-cmd", &object(installed))?;
        Ok(())
    })();
    clear_graph();
    drop(php);
    result
}
