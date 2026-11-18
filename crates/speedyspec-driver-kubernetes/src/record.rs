//! The install record: what an install put in the cluster, kept in the cluster, so it can be
//! found and removed later without the plugin that produced it.

use base64::Engine as _;
use serde_json::{Value as Json, json};
use speedyspec_driver::PluginRef;

use crate::manifest::{APP_LABEL, INSTALL_LABEL, MANAGED_BY, MANAGER, ResourceId, record_name};

/// The version of the record's layout this binary writes and can read.
pub const SCHEMA: u32 = 1;
/// The type of the Secret holding a record.
pub const SECRET_TYPE: &str = "speedyspec.com/install";
const KEY: &str = "record";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Installing,
    Installed,
    Failed,
}

/// A resource an install is responsible for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    pub id: ResourceId,
    /// Applied by an earlier install under this name and no longer returned by the plugin. It
    /// is left running, and removed with the rest on uninstall.
    pub retained: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub schema: u32,
    pub name: String,
    pub app: String,
    pub parts: Vec<String>,
    pub status: Status,
    /// Why, when the status is `Failed`.
    pub message: Option<String>,
    pub cli_version: String,
    pub plugins: Vec<PluginRef>,
    /// Namespaces this install created, and so may delete.
    pub created_namespaces: Vec<String>,
    pub resources: Vec<Recorded>,
}

impl Record {
    /// The Secret manifest that holds this record in `namespace`.
    pub fn to_secret(&self, namespace: &str) -> Json {
        let resources: Vec<Json> = self
            .resources
            .iter()
            .map(|recorded| {
                json!({
                    "apiVersion": recorded.id.api_version,
                    "kind": recorded.id.kind,
                    "namespace": recorded.id.namespace,
                    "name": recorded.id.name,
                    "retained": recorded.retained,
                })
            })
            .collect();
        let plugins: Vec<Json> = self
            .plugins
            .iter()
            .map(|plugin| json!({"name": plugin.name, "version": plugin.version}))
            .collect();
        let record = json!({
            "schema": self.schema,
            "name": self.name,
            "app": self.app,
            "parts": self.parts,
            "status": match self.status {
                Status::Installing => "installing",
                Status::Installed => "installed",
                Status::Failed => "failed",
            },
            "message": self.message,
            "cliVersion": self.cli_version,
            "plugins": plugins,
            "createdNamespaces": self.created_namespaces,
            "resources": resources,
        });
        json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "type": SECRET_TYPE,
            "metadata": {
                "name": record_name(&self.name),
                "namespace": namespace,
                "labels": {
                    MANAGED_BY: MANAGER,
                    APP_LABEL: self.app,
                    INSTALL_LABEL: self.name,
                },
            },
            "stringData": {KEY: record.to_string()},
        })
    }

    /// Reads a record back out of its Secret: as this binary writes it (`stringData`) or as the
    /// cluster returns it (`data`, base64).
    pub fn from_secret(secret: &Json) -> Result<Record, String> {
        if secret["kind"] != "Secret" || secret["type"] != SECRET_TYPE {
            return Err("not a speedyspec install record".to_owned());
        }
        let text = match (
            secret["stringData"][KEY].as_str(),
            secret["data"][KEY].as_str(),
        ) {
            (Some(text), _) => text.to_owned(),
            (None, Some(encoded)) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| "the install record is not valid base64".to_owned())?;
                String::from_utf8(bytes)
                    .map_err(|_| "the install record is not valid UTF-8".to_owned())?
            }
            (None, None) => return Err("the install record is empty".to_owned()),
        };
        let record: Json = serde_json::from_str(&text)
            .map_err(|e| format!("the install record cannot be read: {e}"))?;
        let schema = record["schema"]
            .as_u64()
            .and_then(|schema| u32::try_from(schema).ok())
            .ok_or("the install record has no schema version")?;
        if schema > SCHEMA {
            return Err(format!(
                "the install record was written by a newer speedyspec (schema {schema}, this \
                 one reads up to {SCHEMA})"
            ));
        }
        let text = |value: &Json, what: &str| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("the install record has no {what}"))
        };
        let strings = |value: &Json| -> Vec<String> {
            value
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let status = match record["status"].as_str() {
            Some("installing") => Status::Installing,
            Some("installed") => Status::Installed,
            Some("failed") => Status::Failed,
            _ => return Err("the install record has no status".to_owned()),
        };
        let mut resources = Vec::new();
        for item in record["resources"].as_array().into_iter().flatten() {
            resources.push(Recorded {
                id: ResourceId {
                    api_version: text(&item["apiVersion"], "resource apiVersion")?,
                    kind: text(&item["kind"], "resource kind")?,
                    namespace: item["namespace"].as_str().map(str::to_owned),
                    name: text(&item["name"], "resource name")?,
                },
                retained: item["retained"].as_bool().unwrap_or(false),
            });
        }
        let mut plugins = Vec::new();
        for item in record["plugins"].as_array().into_iter().flatten() {
            plugins.push(PluginRef {
                name: text(&item["name"], "plugin name")?,
                version: item["version"].as_str().unwrap_or_default().to_owned(),
            });
        }
        Ok(Record {
            schema,
            name: text(&record["name"], "name")?,
            app: text(&record["app"], "app")?,
            parts: strings(&record["parts"]),
            status,
            message: record["message"].as_str().map(str::to_owned),
            cli_version: record["cliVersion"].as_str().unwrap_or_default().to_owned(),
            plugins,
            created_namespaces: strings(&record["createdNamespaces"]),
            resources,
        })
    }

    /// The record for a new install over `previous`. Resources the new install no longer
    /// returns are kept after the new ones, marked retained; namespaces the earlier install
    /// created stay recorded.
    pub fn superseding(previous: Option<&Record>, mut fresh: Record) -> Record {
        let Some(previous) = previous else {
            return fresh;
        };
        for earlier in &previous.resources {
            if !fresh.resources.iter().any(|now| now.id == earlier.id) {
                fresh.resources.push(Recorded {
                    id: earlier.id.clone(),
                    retained: true,
                });
            }
        }
        let mut namespaces = previous.created_namespaces.clone();
        for namespace in fresh.created_namespaces {
            if !namespaces.contains(&namespace) {
                namespaces.push(namespace);
            }
        }
        fresh.created_namespaces = namespaces;
        fresh
    }

    /// The retained resources, for the report.
    pub fn dropped(&self) -> Vec<&ResourceId> {
        self.resources
            .iter()
            .filter(|recorded| recorded.retained)
            .map(|recorded| &recorded.id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(kind: &str, name: &str) -> ResourceId {
        ResourceId {
            api_version: "v1".to_owned(),
            kind: kind.to_owned(),
            namespace: Some("store".to_owned()),
            name: name.to_owned(),
        }
    }

    fn fresh(names: &[&str]) -> Record {
        Record {
            schema: SCHEMA,
            name: "prod".to_owned(),
            app: "shop".to_owned(),
            parts: vec!["web".to_owned()],
            status: Status::Installing,
            message: None,
            cli_version: "0.1.0".to_owned(),
            plugins: vec![PluginRef {
                name: "acme/shop".to_owned(),
                version: "1.2.0".to_owned(),
            }],
            created_namespaces: Vec::new(),
            resources: names
                .iter()
                .map(|name| Recorded {
                    id: id("ConfigMap", name),
                    retained: false,
                })
                .collect(),
        }
    }

    fn names(record: &Record) -> Vec<String> {
        record
            .resources
            .iter()
            .map(|recorded| {
                let mark = if recorded.retained { "(retained)" } else { "" };
                format!("{}{mark}", recorded.id.name)
            })
            .collect()
    }

    #[test]
    fn round_trips_through_a_secret() {
        let mut record = fresh(&["a", "b"]);
        record.status = Status::Failed;
        record.message = Some("Deployment/web: not ready".to_owned());
        record.created_namespaces = vec!["store".to_owned()];
        record.resources[1].retained = true;
        record.resources.push(Recorded {
            id: ResourceId {
                api_version: "v1".to_owned(),
                kind: "Namespace".to_owned(),
                namespace: None,
                name: "extra".to_owned(),
            },
            retained: false,
        });
        let secret = record.to_secret("store");
        assert_eq!(Record::from_secret(&secret).unwrap(), record);
    }

    #[test]
    fn secret_has_the_type_and_labels() {
        let secret = fresh(&["a"]).to_secret("store");
        assert_eq!(secret["apiVersion"], "v1");
        assert_eq!(secret["kind"], "Secret");
        assert_eq!(secret["type"], "speedyspec.com/install");
        assert_eq!(secret["metadata"]["name"], "speedyspec-install-prod");
        assert_eq!(secret["metadata"]["namespace"], "store");
        assert_eq!(
            secret["metadata"]["labels"],
            json!({"app.kubernetes.io/managed-by": "speedyspec", "speedyspec.com/app": "shop",
                   "speedyspec.com/install": "prod"})
        );
        assert!(secret["stringData"]["record"].is_string());
    }

    #[test]
    fn reads_a_secret_as_the_cluster_returns_it() {
        // The API server stores stringData as base64 under data.
        let record = fresh(&["a"]);
        let mut secret = record.to_secret("store");
        let text = secret["stringData"]["record"].as_str().unwrap().to_owned();
        secret.as_object_mut().unwrap().remove("stringData");
        secret["data"] = json!({"record": base64(text.as_bytes())});
        assert_eq!(Record::from_secret(&secret).unwrap(), record);
    }

    fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    #[test]
    fn refuses_a_foreign_secret_and_a_newer_schema() {
        let mut other = fresh(&["a"]).to_secret("store");
        other["type"] = json!("Opaque");
        let error = Record::from_secret(&other).unwrap_err();
        assert!(error.contains("not a speedyspec install record"), "{error}");

        let mut empty = fresh(&["a"]).to_secret("store");
        empty["stringData"] = json!({});
        assert!(Record::from_secret(&empty).is_err());

        let mut garbled = fresh(&["a"]).to_secret("store");
        garbled["stringData"]["record"] = json!("{not json");
        assert!(Record::from_secret(&garbled).is_err());

        let mut newer = fresh(&["a"]);
        newer.schema = SCHEMA + 1;
        let error = Record::from_secret(&newer.to_secret("store")).unwrap_err();
        assert!(error.contains("newer speedyspec"), "{error}");
    }

    #[test]
    fn first_install_has_nothing_retained() {
        let record = Record::superseding(None, fresh(&["a", "b"]));
        assert_eq!(names(&record), vec!["a", "b"]);
        assert!(record.dropped().is_empty());
    }

    #[test]
    fn dropped_resources_are_retained_after_the_new_ones() {
        let previous = fresh(&["a", "b", "c"]);
        let record = Record::superseding(Some(&previous), fresh(&["b", "d"]));
        assert_eq!(names(&record), vec!["b", "d", "a(retained)", "c(retained)"]);
        let dropped: Vec<&str> = record.dropped().iter().map(|id| id.name.as_str()).collect();
        assert_eq!(dropped, vec!["a", "c"]);
    }

    #[test]
    fn a_returning_resource_is_no_longer_retained() {
        let mut previous = fresh(&["a", "b"]);
        previous.resources[0].retained = true;
        let record = Record::superseding(Some(&previous), fresh(&["a"]));
        assert_eq!(names(&record), vec!["a", "b(retained)"]);
    }

    #[test]
    fn retained_resources_stay_retained_across_installs() {
        let first = Record::superseding(Some(&fresh(&["a", "b"])), fresh(&["b"]));
        let second = Record::superseding(Some(&first), fresh(&["b"]));
        assert_eq!(names(&second), vec!["b", "a(retained)"]);
    }

    #[test]
    fn created_namespaces_accumulate() {
        let mut previous = fresh(&["a"]);
        previous.created_namespaces = vec!["store".to_owned()];
        let mut next = fresh(&["a"]);
        next.created_namespaces = vec!["data".to_owned(), "store".to_owned()];
        let record = Record::superseding(Some(&previous), next);
        assert_eq!(record.created_namespaces, vec!["store", "data"]);
    }
}
