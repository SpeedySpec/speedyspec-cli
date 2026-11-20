//! The install sequence: check everything, make the namespaces, write the record, apply.
//!
//! The record is written before the first resource is applied and lists everything about to be
//! applied, so an install that fails part-way can still be found and removed.

use serde_json::Value as Json;
use std::time::{Duration, Instant};

use speedyspec_driver::{KubernetesResources, PluginRef, Progress, WaitFor};

use crate::cluster::ClusterError;
use crate::connect::Connection;
use crate::manifest::{InstallIdentity, ResourceId, prepare_manifest, record_name};
use crate::readiness::{Readiness, default_wait, readiness};
use crate::record::{Record, Recorded, SCHEMA, Status};

/// What an install is, beyond the resources.
pub struct InstallOptions<'a> {
    pub identity: InstallIdentity<'a>,
    pub parts: &'a [String],
    pub force_conflicts: bool,
    pub plugins: &'a [PluginRef],
    pub cli_version: &'a str,
}

/// What an install put in the cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub resources: Vec<ResourceId>,
    /// Applied by an earlier install under this name, no longer returned, and left running.
    pub retained: Vec<ResourceId>,
}

pub(crate) fn record_id(identity: &InstallIdentity<'_>) -> ResourceId {
    ResourceId {
        api_version: "v1".to_owned(),
        kind: "Secret".to_owned(),
        namespace: Some(identity.namespace.to_owned()),
        name: record_name(identity.name),
    }
}

/// The record of the install called `identity.name`, if there is one.
pub(crate) fn read_record(
    connection: &Connection,
    identity: &InstallIdentity<'_>,
) -> Result<Option<Record>, String> {
    let id = record_id(identity);
    match connection.get(&id) {
        Ok(secret) => Record::from_secret(&secret)
            .map(Some)
            .map_err(|problem| format!("{}: {problem}", id.display())),
        Err(ClusterError::NotFound) => Ok(None),
        Err(error) => Err(error.message(&id.display())),
    }
}

pub(crate) fn write_record(
    connection: &Connection,
    record: &Record,
    namespace: &str,
) -> Result<(), String> {
    // The record is this tool's own; nobody else's claim on its fields stands.
    connection
        .apply(&record.to_secret(namespace), true)
        .map_err(|error| {
            format!(
                "could not write the install record: {}",
                error.message(&record_name(&record.name))
            )
        })
}

/// Records how far the app's setup got: `Files` with the tasks finished so far, or `Installed` once all have.
pub fn record_setup(
    connection: &Connection,
    name: &str,
    namespace: &str,
    status: Status,
    done: u64,
) -> Result<(), String> {
    let identity = InstallIdentity {
        app: "",
        name,
        namespace,
    };
    let mut record = read_record(connection, &identity)?
        .ok_or_else(|| format!("no install named {name} in namespace {namespace}"))?;
    record.status = status;
    record.setup_done = Some(done);
    write_record(connection, &record, namespace)
}

/// Applies an install. Nothing is waited for; see `wait_and_finish`.
pub fn apply_install(
    connection: &Connection,
    answer: &KubernetesResources,
    options: &InstallOptions<'_>,
    progress: &mut dyn Progress,
) -> Result<Applied, String> {
    let identity = &options.identity;
    if answer.resources.is_empty() {
        return Err("no plugin supplied anything to install".to_owned());
    }
    let previous = read_record(connection, identity)?;
    if let Some(previous) = &previous
        && previous.app != identity.app
    {
        return Err(format!(
            "an install named {} already exists in namespace {} for the app {}, not {}; choose \
             another --name",
            identity.name, identity.namespace, previous.app, identity.app
        ));
    }

    // Everything is checked before the cluster is changed.
    let mut manifests: Vec<(ResourceId, Json)> = Vec::with_capacity(answer.resources.len());
    // Kinds defined earlier in this same answer: the cluster cannot say anything about them
    // until their definition has been applied, so the definition is read instead.
    let mut defined: Vec<(String, String, bool)> = Vec::new();
    let mut late: Vec<ResourceId> = Vec::new();
    for resource in &answer.resources {
        let mut manifest = resource.clone();
        let api_version = manifest["apiVersion"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let kind = manifest["kind"].as_str().unwrap_or_default().to_owned();
        let group = api_version.split_once('/').map_or("", |(group, _)| group);
        let from_definition = defined
            .iter()
            .find(|(defined_group, defined_kind, _)| {
                defined_group == group && *defined_kind == kind
            })
            .map(|(_, _, namespaced)| *namespaced);
        let namespaced = match from_definition {
            Some(namespaced) => namespaced,
            None => connection
                .scope(&api_version, &kind)
                .map_err(|error| error.message(&kind))?,
        };
        if let Some(definition) = definition(&manifest) {
            defined.push(definition);
        }
        let id = prepare_manifest(&mut manifest, namespaced, identity)?;
        if from_definition.is_some() {
            late.push(id.clone());
        }
        manifests.push((id, manifest));
    }

    // Nothing that is already in the cluster and is not this install's is taken over: it would
    // be labelled as the install's and deleted with it.
    let mut taken = Vec::new();
    for (id, _) in manifests.iter().filter(|(id, _)| !late.contains(id)) {
        if let Some(problem) = someone_elses(connection, id, previous.as_ref())? {
            taken.push(problem);
        }
    }
    if !taken.is_empty() {
        return Err(taken.join("\n"));
    }

    let mut record = Record::superseding(
        previous.as_ref(),
        Record {
            schema: SCHEMA,
            name: identity.name.to_owned(),
            app: identity.app.to_owned(),
            parts: options.parts.to_vec(),
            status: Status::Installing,
            message: None,
            cli_version: options.cli_version.to_owned(),
            plugins: options.plugins.to_vec(),
            created_namespaces: Vec::new(),
            resources: manifests
                .iter()
                .map(|(id, _)| Recorded {
                    id: id.clone(),
                    retained: false,
                })
                .collect(),
            setup_done: None,
        },
    );

    let outcome = place(
        connection,
        &manifests,
        (&late, previous.as_ref()),
        &mut record,
        options,
        progress,
    );
    if let Err(problem) = outcome {
        record.status = Status::Failed;
        record.message = Some(problem.clone());
        return Err(
            match write_record(connection, &record, identity.namespace) {
                Ok(()) => problem,
                Err(unrecorded) => format!("{problem}\n{unrecorded}"),
            },
        );
    }
    Ok(Applied {
        resources: manifests.into_iter().map(|(id, _)| id).collect(),
        retained: record.dropped().into_iter().cloned().collect(),
    })
}

/// The group, kind and scope a CustomResourceDefinition defines.
fn definition(manifest: &Json) -> Option<(String, String, bool)> {
    let is_definition = manifest["kind"] == "CustomResourceDefinition"
        && manifest["apiVersion"]
            .as_str()
            .is_some_and(|version| version.starts_with("apiextensions.k8s.io/"));
    if !is_definition {
        return None;
    }
    let spec = &manifest["spec"];
    Some((
        spec["group"].as_str()?.to_owned(),
        spec["names"]["kind"].as_str()?.to_owned(),
        spec["scope"].as_str() != Some("Cluster"),
    ))
}

/// Why `id` cannot be installed over, when it is in the cluster already and no earlier install
/// under this name put it there.
fn someone_elses(
    connection: &Connection,
    id: &ResourceId,
    previous: Option<&Record>,
) -> Result<Option<String>, String> {
    let ours = previous.is_some_and(|record| {
        record.resources.iter().any(|recorded| recorded.id == *id)
            || (id.kind == "Namespace"
                && id.api_version == "v1"
                && record.created_namespaces.contains(&id.name))
    });
    if ours {
        return Ok(None);
    }
    match connection.get(id) {
        Ok(_) => Ok(Some(format!(
            "{} already exists and is not part of this install; it is left as it is. Delete \
             it, or have the plugin leave it out",
            id.display()
        ))),
        Err(ClusterError::NotFound) => Ok(None),
        Err(error) => Err(error.message(&id.display())),
    }
}

/// Makes the namespaces, writes the record, applies the resources.
fn place(
    connection: &Connection,
    manifests: &[(ResourceId, Json)],
    (late, previous): (&[ResourceId], Option<&Record>),
    record: &mut Record,
    options: &InstallOptions<'_>,
    progress: &mut dyn Progress,
) -> Result<(), String> {
    let identity = &options.identity;
    let mut namespaces: Vec<&str> = vec![identity.namespace];
    for (id, _) in manifests {
        if let Some(namespace) = &id.namespace
            && !namespaces.contains(&namespace.as_str())
        {
            namespaces.push(namespace);
        }
    }
    for namespace in namespaces {
        let label = format!("Namespace/{namespace}");
        let exists = connection
            .namespace_exists(namespace)
            .map_err(|error| error.message(&label))?;
        if exists {
            continue;
        }
        progress.step(&format!("creating namespace {namespace}"));
        connection
            .create_namespace(namespace, identity)
            .map_err(|error| error.message(&label))?;
        if !record.created_namespaces.iter().any(|n| n == namespace) {
            record.created_namespaces.push(namespace.to_owned());
        }
    }

    write_record(connection, record, identity.namespace)?;

    for (id, manifest) in manifests {
        // A kind defined by this install could only be asked about now.
        if late.contains(id)
            && let Some(problem) = someone_elses(connection, id, previous)?
        {
            return Err(problem);
        }
        progress.step(&format!("applying {}", id.display()));
        connection
            .apply(manifest, options.force_conflicts)
            .map_err(|error| error.message(&id.display()))?;
    }
    Ok(())
}

/// Waits for the install's workloads, then marks the record installed, or failed with what was
/// not ready and why. Nothing is rolled back.
pub fn wait_and_finish(
    connection: &Connection,
    applied: &Applied,
    wait_for: Option<&[WaitFor]>,
    identity: &InstallIdentity<'_>,
    timeout: Duration,
    interval: Duration,
    progress: &mut dyn Progress,
) -> Result<(), String> {
    let outcome = targets(applied, wait_for)
        .and_then(|targets| wait(connection, &targets, timeout, interval, progress));
    let mut record = read_record(connection, identity)?
        .ok_or_else(|| format!("the record of install {} has disappeared", identity.name))?;
    match &outcome {
        Ok(()) => {
            record.status = Status::Installed;
            record.message = None;
        }
        Err(problem) => {
            record.status = Status::Failed;
            record.message = Some(problem.clone());
        }
    }
    let written = write_record(connection, &record, identity.namespace);
    match (outcome, written) {
        (Ok(()), written) => written,
        (Err(problem), Ok(())) => Err(problem),
        (Err(problem), Err(unrecorded)) => Err(format!("{problem}\n{unrecorded}")),
    }
}

/// What to wait on, with the condition to wait for where one was given.
fn targets(
    applied: &Applied,
    wait_for: Option<&[WaitFor]>,
) -> Result<Vec<(ResourceId, Option<String>)>, String> {
    let Some(wait_for) = wait_for else {
        return Ok(default_wait(&applied.resources)
            .into_iter()
            .map(|id| (id, None))
            .collect());
    };
    wait_for
        .iter()
        .map(|wait| {
            applied
                .resources
                .iter()
                .find(|id| {
                    id.kind == wait.kind
                        && id.name == wait.name
                        && wait
                            .namespace
                            .as_ref()
                            .is_none_or(|ns| id.namespace.as_ref() == Some(ns))
                })
                .map(|id| (id.clone(), wait.condition.clone()))
                .ok_or_else(|| {
                    format!(
                        "cannot wait for {} \"{}\": it was not installed",
                        wait.kind, wait.name
                    )
                })
        })
        .collect()
}

fn wait(
    connection: &Connection,
    targets: &[(ResourceId, Option<String>)],
    timeout: Duration,
    interval: Duration,
    progress: &mut dyn Progress,
) -> Result<(), String> {
    let started = Instant::now();
    // The last reason reported for each target, so a line is printed only when it changes.
    let mut reasons: Vec<Option<String>> = vec![None; targets.len()];
    loop {
        let mut waiting = Vec::new();
        for (index, (id, condition)) in targets.iter().enumerate() {
            let state = match connection.get(id) {
                Ok(object) => readiness(&id.kind, &object, condition.as_deref()),
                Err(ClusterError::NotFound) => Readiness::NotYet("not found yet".to_owned()),
                Err(error) => Readiness::NotYet(error.message(&id.display())),
            };
            match state {
                Readiness::Ready => {
                    if reasons[index].take().is_some() {
                        progress.step(&format!("{} is ready", id.display()));
                    }
                }
                Readiness::Failed(reason) => {
                    return Err(format!("{} failed: {reason}", id.display()));
                }
                Readiness::NotYet(reason) => {
                    if reasons[index].as_deref() != Some(reason.as_str()) {
                        progress.step(&format!("waiting for {}: {reason}", id.display()));
                        reasons[index] = Some(reason.clone());
                    }
                    waiting.push(format!("{} ({reason})", id.display()));
                }
            }
        }
        if waiting.is_empty() {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "not ready after {}s: {}",
                timeout.as_secs(),
                waiting.join(", ")
            ));
        }
        std::thread::sleep(interval);
    }
}
