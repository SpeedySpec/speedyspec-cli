//! The install sequence: check everything, make the namespaces, write the record, apply.
//!
//! The record is written before the first resource is applied and lists everything about to be
//! applied, so an install that fails part-way can still be found and removed.

use serde_json::Value as Json;
use speedyspec_driver::{KubernetesResources, PluginRef, Progress};

use crate::cluster::ClusterError;
use crate::connect::Connection;
use crate::manifest::{InstallIdentity, ResourceId, prepare_manifest, record_name};
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
    for resource in &answer.resources {
        let mut manifest = resource.clone();
        let api_version = manifest["apiVersion"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let kind = manifest["kind"].as_str().unwrap_or_default().to_owned();
        let namespaced = connection
            .scope(&api_version, &kind)
            .map_err(|error| error.message(&kind))?;
        let id = prepare_manifest(&mut manifest, namespaced, identity)?;
        manifests.push((id, manifest));
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
        },
    );

    let outcome = place(connection, &manifests, &mut record, options, progress);
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

/// Makes the namespaces, writes the record, applies the resources.
fn place(
    connection: &Connection,
    manifests: &[(ResourceId, Json)],
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
        progress.step(&format!("applying {}", id.display()));
        connection
            .apply(manifest, options.force_conflicts)
            .map_err(|error| error.message(&id.display()))?;
    }
    Ok(())
}
