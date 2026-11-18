//! Removing an install: what its record lists, and nothing else.

use std::time::{Duration, Instant};

use speedyspec_driver::Progress;

use crate::cluster::{ClusterError, namespace_id};
use crate::connect::Connection;
use crate::install::{read_record, record_id};
use crate::manifest::{INSTALL_LABEL, InstallIdentity, ResourceId};
use crate::record::Record;

/// An install found in the cluster, ready to be removed.
pub struct Removal {
    pub record: Record,
    /// Where the record lives.
    pub namespace: String,
}

/// What an uninstall did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub deleted: Vec<ResourceId>,
    pub already_gone: Vec<ResourceId>,
    /// Left alone, and why.
    pub skipped: Vec<(ResourceId, String)>,
    /// Namespaces deleted.
    pub namespaces: Vec<String>,
    /// Namespaces the install created but that were kept, and why.
    pub kept_namespaces: Vec<(String, String)>,
}

/// Finds the install called `name` whose record is in `namespace`.
pub fn find_install(
    connection: &Connection,
    name: &str,
    namespace: &str,
) -> Result<Removal, String> {
    let identity = InstallIdentity {
        app: "",
        name,
        namespace,
    };
    match read_record(connection, &identity)? {
        Some(record) => Ok(Removal {
            record,
            namespace: namespace.to_owned(),
        }),
        None => Err(format!("no install named {name} in namespace {namespace}")),
    }
}

/// What removing the install will delete, for the user to read before confirming.
pub fn summarise(removal: &Removal) -> Vec<String> {
    let record = &removal.record;
    let mut kinds: Vec<(&str, usize)> = Vec::new();
    for recorded in &record.resources {
        match kinds.iter_mut().find(|(kind, _)| *kind == recorded.id.kind) {
            Some((_, count)) => *count += 1,
            None => kinds.push((&recorded.id.kind, 1)),
        }
    }
    let counted: Vec<String> = kinds
        .iter()
        .map(|(kind, count)| format!("{count} {kind}"))
        .collect();
    let mut lines = vec![format!(
        "uninstalling {} (app {}) from namespace {}: {}",
        record.name,
        record.app,
        removal.namespace,
        if counted.is_empty() {
            "no resources".to_owned()
        } else {
            counted.join(", ")
        }
    )];
    for namespace in &record.created_namespaces {
        lines.push(format!(
            "this deletes namespace {namespace}, which the install created, and everything in it"
        ));
    }
    let claims = kinds
        .iter()
        .find(|(kind, _)| *kind == "PersistentVolumeClaim")
        .map_or(0, |(_, count)| *count);
    if claims > 0 {
        lines.push(format!(
            "this deletes {claims} PersistentVolumeClaim(s) and the data in them"
        ));
    }
    lines
}

/// Whether the object still says it belongs to the install called `name`.
fn belongs(object: &serde_json::Value, name: &str) -> bool {
    object["metadata"]["labels"][INSTALL_LABEL] == name
}

/// Deletes the install's resources in reverse order, then the namespaces it created, then its
/// record. A failure stops there with the record still in place, so it can be run again.
pub fn remove_install(
    connection: &Connection,
    removal: &Removal,
    timeout: Duration,
    interval: Duration,
    progress: &mut dyn Progress,
) -> Result<Removed, String> {
    let record = &removal.record;
    let mut removed = Removed::default();

    for recorded in record.resources.iter().rev() {
        let id = &recorded.id;
        match connection.get(id) {
            Err(ClusterError::NotFound | ClusterError::UnknownKind(_)) => {
                removed.already_gone.push(id.clone());
            }
            Err(error) => return Err(error.message(&id.display())),
            // Same name, but something else now: it was replaced, and is not this install's
            // to delete.
            Ok(object) if !belongs(&object, &record.name) => {
                removed
                    .skipped
                    .push((id.clone(), "no longer part of this install".to_owned()));
            }
            Ok(_) => {
                progress.step(&format!("deleting {}", id.display()));
                match connection.delete(id) {
                    Ok(()) | Err(ClusterError::NotFound) => removed.deleted.push(id.clone()),
                    Err(error) => return Err(error.message(&id.display())),
                }
            }
        }
    }
    wait_until_gone(connection, &removed.deleted, timeout, interval)?;

    // Deleting a namespace deletes what is in it, so one that still holds something this
    // uninstall refused to delete stays.
    let holds_skipped = |namespace: &str, skipped: &[(ResourceId, String)]| {
        skipped
            .iter()
            .find(|(id, _)| id.namespace.as_deref() == Some(namespace))
            .map(|(id, _)| {
                format!(
                    "it still holds {}, which is not this install's",
                    id.display()
                )
            })
    };
    let mut created: Vec<&String> = record
        .created_namespaces
        .iter()
        .filter(|namespace| **namespace != removal.namespace)
        .collect();
    let own = record
        .created_namespaces
        .iter()
        .find(|namespace| **namespace == removal.namespace);

    for namespace in created.drain(..) {
        if let Some(why) = holds_skipped(namespace, &removed.skipped) {
            removed.kept_namespaces.push((namespace.clone(), why));
        } else if delete_namespace(
            connection,
            namespace,
            &record.name,
            timeout,
            interval,
            progress,
        )? {
            removed.namespaces.push(namespace.clone());
        }
    }

    // The record goes only now: everything before it can be run again.
    let record_id = record_id(&InstallIdentity {
        app: &record.app,
        name: &record.name,
        namespace: &removal.namespace,
    });
    match connection.delete(&record_id) {
        Ok(()) | Err(ClusterError::NotFound) => {}
        Err(error) => return Err(error.message(&record_id.display())),
    }

    // The record's own namespace, if the install created it, goes last.
    if let Some(namespace) = own {
        if let Some(why) = holds_skipped(namespace, &removed.skipped) {
            removed.kept_namespaces.push((namespace.clone(), why));
        } else if delete_namespace(
            connection,
            namespace,
            &record.name,
            timeout,
            interval,
            progress,
        )? {
            removed.namespaces.push(namespace.clone());
        }
    }
    Ok(removed)
}

/// Deletes a namespace the install created, if it is still the install's. Returns whether it
/// was deleted.
fn delete_namespace(
    connection: &Connection,
    namespace: &str,
    install: &str,
    timeout: Duration,
    interval: Duration,
    progress: &mut dyn Progress,
) -> Result<bool, String> {
    let id = namespace_id(namespace);
    match connection.get(&id) {
        Err(ClusterError::NotFound) => return Ok(false),
        Err(error) => return Err(error.message(&id.display())),
        Ok(object) if !belongs(&object, install) => return Ok(false),
        Ok(_) => {}
    }
    progress.step(&format!("deleting namespace {namespace}"));
    match connection.delete(&id) {
        Ok(()) | Err(ClusterError::NotFound) => {}
        Err(error) => return Err(error.message(&id.display())),
    }
    wait_until_gone(connection, std::slice::from_ref(&id), timeout, interval)?;
    Ok(true)
}

/// Waits until none of `ids` is in the cluster any more.
fn wait_until_gone(
    connection: &Connection,
    ids: &[ResourceId],
    timeout: Duration,
    interval: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let mut remaining = Vec::new();
        for id in ids {
            match connection.get(id) {
                Err(ClusterError::NotFound | ClusterError::UnknownKind(_)) => {}
                Ok(_) => remaining.push(id.display()),
                Err(error) => return Err(error.message(&id.display())),
            }
        }
        if remaining.is_empty() {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "still being deleted after {}s: {}",
                timeout.as_secs(),
                remaining.join(", ")
            ));
        }
        std::thread::sleep(interval);
    }
}
