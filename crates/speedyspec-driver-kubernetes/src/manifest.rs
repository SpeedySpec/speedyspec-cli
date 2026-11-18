//! What a manifest is (its identity in a cluster) and what the CLI adds to it before applying.

use serde_json::{Value as Json, json};

/// What makes a resource one particular thing in a cluster.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResourceId {
    pub api_version: String,
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
}

impl ResourceId {
    /// The API group: empty for the core group (`v1`).
    pub fn group(&self) -> &str {
        match self.api_version.split_once('/') {
            Some((group, _)) => group,
            None => "",
        }
    }

    fn group_is_core(&self) -> bool {
        self.group().is_empty()
    }

    /// For messages: `Deployment/web`, or `shop/Deployment/web` when it has a namespace.
    pub fn display(&self) -> String {
        match &self.namespace {
            Some(namespace) => format!("{namespace}/{}/{}", self.kind, self.name),
            None => format!("{}/{}", self.kind, self.name),
        }
    }
}

/// Which install a resource belongs to.
#[derive(Clone, Copy, Debug)]
pub struct InstallIdentity<'a> {
    pub app: &'a str,
    /// The install's name.
    pub name: &'a str,
    /// Where the record lives, and the default for resources that name no namespace.
    pub namespace: &'a str,
}

pub const MANAGED_BY: &str = "app.kubernetes.io/managed-by";
pub const APP_LABEL: &str = "speedyspec.com/app";
pub const INSTALL_LABEL: &str = "speedyspec.com/install";
/// The field manager of every apply, and the value of the managed-by label.
pub const MANAGER: &str = "speedyspec";

/// The identity a manifest states, as written. The contract check has already made sure the
/// fields are there.
pub fn identify(manifest: &Json) -> ResourceId {
    let text = |value: &Json| value.as_str().unwrap_or_default().to_owned();
    ResourceId {
        api_version: text(&manifest["apiVersion"]),
        kind: text(&manifest["kind"]),
        namespace: manifest["metadata"]["namespace"]
            .as_str()
            .map(str::to_owned),
        name: text(&manifest["metadata"]["name"]),
    }
}

/// The three labels every resource of an install carries.
pub fn labels(identity: &InstallIdentity<'_>) -> Json {
    json!({
        MANAGED_BY: MANAGER,
        APP_LABEL: identity.app,
        INSTALL_LABEL: identity.name,
    })
}

/// Makes a manifest ready to apply: sets the three labels (over a plugin's own values for those
/// keys) and, for a namespaced resource that names no namespace, the install's. Returns the
/// resource's final identity.
pub fn prepare_manifest(
    manifest: &mut Json,
    namespaced: bool,
    identity: &InstallIdentity<'_>,
) -> Result<ResourceId, String> {
    let stated = identify(manifest);
    if !namespaced && stated.namespace.is_some() {
        return Err(format!(
            "{} is cluster-wide and cannot have a namespace",
            stated.kind
        ));
    }
    let namespace = match (namespaced, stated.namespace.clone()) {
        (true, None) => Some(identity.namespace.to_owned()),
        (_, given) => given,
    };
    if stated.kind == "Secret"
        && stated.group_is_core()
        && stated.name == record_name(identity.name)
        && namespace.as_deref() == Some(identity.namespace)
    {
        return Err(format!(
            "Secret \"{}\" in namespace {}: that name is reserved for the install record",
            stated.name, identity.namespace
        ));
    }
    if let Some(namespace) = &namespace {
        manifest["metadata"]["namespace"] = json!(namespace);
    }
    let own = labels(identity);
    let existing = &mut manifest["metadata"]["labels"];
    if !existing.is_object() {
        *existing = json!({});
    }
    for (key, value) in own.as_object().expect("labels are an object") {
        existing[key] = value.clone();
    }
    Ok(ResourceId {
        namespace,
        ..identify(manifest)
    })
}

/// Why `name` cannot name an install, if it cannot: it becomes part of a Secret's name and a
/// label value, so it must be a DNS label, and short enough to leave room for the prefix.
pub fn install_name_problem(name: &str) -> Option<String> {
    dns_label_problem(name, 40)
}

/// Why `name` cannot be a namespace, if it cannot.
pub fn namespace_problem(name: &str) -> Option<String> {
    dns_label_problem(name, 63)
}

/// Why `name` is not a DNS label of at most `longest` characters, if it is not.
fn dns_label_problem(name: &str, longest: usize) -> Option<String> {
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let inside = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if name.is_empty()
        || name.len() > longest
        || !inside
        || !edge(name.chars().next())
        || !edge(name.chars().last())
    {
        return Some(format!(
            "\"{name}\" must be 1 to {longest} lowercase letters, digits or hyphens, starting \
             and ending with a letter or digit"
        ));
    }
    None
}

/// The name of the Secret that records an install.
pub fn record_name(install_name: &str) -> String {
    format!("speedyspec-install-{install_name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: InstallIdentity<'static> = InstallIdentity {
        app: "shop",
        name: "prod",
        namespace: "store",
    };

    fn deployment() -> Json {
        json!({"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": "web"}})
    }

    #[test]
    fn identifies_core_and_grouped_resources() {
        let core = identify(&json!({"apiVersion": "v1", "kind": "ConfigMap",
                                    "metadata": {"name": "a", "namespace": "n"}}));
        assert_eq!(core.group(), "");
        assert_eq!(core.display(), "n/ConfigMap/a");
        let grouped = identify(&deployment());
        assert_eq!(grouped.group(), "apps");
        assert_eq!(grouped.namespace, None);
        assert_eq!(grouped.display(), "Deployment/web");
    }

    #[test]
    fn sets_labels_over_the_plugins_own() {
        let mut manifest = json!({"apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": "web", "labels": {"app.kubernetes.io/managed-by": "helm", "tier": "web"}}});
        prepare_manifest(&mut manifest, true, &IDENTITY).unwrap();
        assert_eq!(
            manifest["metadata"]["labels"],
            json!({"app.kubernetes.io/managed-by": "speedyspec", "speedyspec.com/app": "shop",
                   "speedyspec.com/install": "prod", "tier": "web"})
        );
    }

    #[test]
    fn defaults_the_namespace_only_when_namespaced_and_absent() {
        let mut manifest = deployment();
        let id = prepare_manifest(&mut manifest, true, &IDENTITY).unwrap();
        assert_eq!(id.namespace.as_deref(), Some("store"));
        assert_eq!(manifest["metadata"]["namespace"], "store");

        let mut elsewhere = json!({"apiVersion": "v1", "kind": "ConfigMap",
                                   "metadata": {"name": "a", "namespace": "data"}});
        let id = prepare_manifest(&mut elsewhere, true, &IDENTITY).unwrap();
        assert_eq!(id.namespace.as_deref(), Some("data"));

        let mut cluster_wide =
            json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "x"}});
        let id = prepare_manifest(&mut cluster_wide, false, &IDENTITY).unwrap();
        assert_eq!(id.namespace, None);
        assert!(cluster_wide["metadata"].get("namespace").is_none());
    }

    #[test]
    fn cluster_wide_resource_with_a_namespace_is_refused() {
        let mut manifest = json!({"apiVersion": "v1", "kind": "Namespace",
                                  "metadata": {"name": "x", "namespace": "store"}});
        let error = prepare_manifest(&mut manifest, false, &IDENTITY).unwrap_err();
        assert_eq!(
            error,
            "Namespace is cluster-wide and cannot have a namespace"
        );
    }

    #[test]
    fn the_record_name_is_reserved() {
        let secret = |namespace: Option<&str>, name: &str| {
            let mut manifest =
                json!({"apiVersion": "v1", "kind": "Secret", "metadata": {"name": name}});
            if let Some(namespace) = namespace {
                manifest["metadata"]["namespace"] = json!(namespace);
            }
            manifest
        };
        let reserved = "speedyspec-install-prod";
        assert_eq!(record_name("prod"), reserved);
        for mut taken in [secret(None, reserved), secret(Some("store"), reserved)] {
            let error = prepare_manifest(&mut taken, true, &IDENTITY).unwrap_err();
            assert!(error.contains("reserved for the install record"), "{error}");
        }
        // The same name elsewhere, or for another kind, is nobody's business.
        assert!(prepare_manifest(&mut secret(Some("data"), reserved), true, &IDENTITY).is_ok());
        let mut config_map =
            json!({"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": reserved}});
        assert!(prepare_manifest(&mut config_map, true, &IDENTITY).is_ok());
    }

    #[test]
    fn install_name_rules() {
        for good in ["site", "a", "prod-2", &"a".repeat(40)] {
            assert_eq!(install_name_problem(good), None, "{good}");
        }
        for bad in ["Site", "a_b", "", "-a", "a-", "a.b", &"a".repeat(41)] {
            assert!(install_name_problem(bad).is_some(), "{bad}");
        }
        assert_eq!(namespace_problem(&"a".repeat(63)), None);
        assert!(namespace_problem(&"a".repeat(64)).is_some());
        assert!(namespace_problem("Shop").is_some());
    }
}
