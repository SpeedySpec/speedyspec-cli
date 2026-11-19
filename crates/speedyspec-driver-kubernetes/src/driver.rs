//! The Kubernetes driver as the CLI sees it.

use std::collections::BTreeMap;
use std::time::Duration;

use speedyspec_driver::{
    ContractError, Driver, DriverError, DriverFlag, DriverInput, KubernetesResources, ObjectReader,
    PluginRef, Prepared, Progress, UninstallInput, UninstallPlan, decode_kubernetes_resources,
};
use speedyspec_hooks::{Key, Value};

use crate::connect::{ConnectError, ConnectFlags, Connection, connect};
use crate::install::{InstallOptions, apply_install, wait_and_finish};
use crate::manifest::{InstallIdentity, install_name_problem, namespace_problem};
use crate::readiness::{KNOWN_KINDS, parse_timeout};
use crate::uninstall::{Removal, find_install, remove_install, summarise};

/// The environment variable a token may be given in.
pub const TOKEN_VARIABLE: &str = "SPEEDYSPEC_KUBE_TOKEN";
/// How often a waiting install looks at its workloads.
const POLL: Duration = Duration::from_millis(500);

/// Applies the resources a plugin returns to a cluster, waits for them and records the install.
#[derive(Default)]
pub struct KubernetesDriver {
    /// Written into each install record.
    cli_version: &'static str,
}

/// What `plan_uninstall` found.
struct Uninstalling {
    connection: Connection,
    removal: Removal,
    timeout: Duration,
}

/// What `prepare` settled for one install.
struct Settings {
    connection: Connection,
    app: String,
    parts: Vec<String>,
    name: String,
    namespace: String,
    timeout: Duration,
    force_conflicts: bool,
}

impl KubernetesDriver {
    pub fn new() -> Self {
        Self {
            cli_version: env!("CARGO_PKG_VERSION"),
        }
    }

    /// The version of the program running the driver, recorded with each install.
    pub fn cli_version(mut self, version: &'static str) -> Self {
        self.cli_version = version;
        self
    }
}

fn value_flag(name: &'static str, help: &'static str) -> DriverFlag {
    DriverFlag {
        name,
        help,
        takes_value: true,
        default: None,
    }
}

fn switch(name: &'static str, help: &'static str) -> DriverFlag {
    DriverFlag {
        name,
        help,
        takes_value: false,
        default: None,
    }
}

/// The flags that say which cluster, and as whom.
pub(crate) fn connection_flags() -> Vec<DriverFlag> {
    vec![
        value_flag("server", "The cluster's API server URL (needs a token)"),
        value_flag(
            "token-file",
            "File holding the token (- for standard input); or set SPEEDYSPEC_KUBE_TOKEN",
        ),
        value_flag(
            "ca-cert",
            "PEM file with the cluster's certificate authority",
        ),
        switch(
            "insecure-skip-tls-verify",
            "Do not check the cluster's certificate",
        ),
        value_flag(
            "context",
            "The kubeconfig context to use when no --server is given",
        ),
    ]
}

/// Opens the connection the flags describe. The token comes from the environment or, for
/// `--token-file -`, standard input.
pub(crate) fn open(flags: &BTreeMap<String, String>) -> Result<Connection, DriverError> {
    let get = |name: &str| flags.get(name).map(String::as_str);
    let env_token = std::env::var(TOKEN_VARIABLE).ok();
    connect(
        &ConnectFlags {
            server: get("server"),
            token_file: get("token-file"),
            ca_cert: get("ca-cert"),
            insecure_skip_tls_verify: flags.contains_key("insecure-skip-tls-verify"),
            context: get("context"),
        },
        env_token.as_deref(),
        &mut std::io::stdin().lock(),
    )
    .map_err(|error| match error {
        ConnectError::Usage(message) | ConnectError::Failure(message) => DriverError(message),
    })
}

/// The flags that find an install in its cluster: which cluster, as whom, and the namespace its record is in. The
/// maintenance commands take these.
pub fn cluster_flags() -> Vec<DriverFlag> {
    let mut flags = connection_flags();
    flags.push(value_flag(
        "namespace",
        "Namespace the install's record is in [kubernetes]",
    ));
    flags
}

/// The connection and the namespace `cluster_flags` give.
pub fn open_cluster(flags: &BTreeMap<String, String>) -> Result<(Connection, String), String> {
    let namespace = namespace_of(flags).map_err(|e| e.0)?;
    let connection = open(flags).map_err(|e| e.0)?;
    Ok((connection, namespace))
}

/// The `--namespace` value, checked.
pub(crate) fn namespace_of(flags: &BTreeMap<String, String>) -> Result<String, DriverError> {
    let namespace = flags.get("namespace").ok_or_else(|| {
        DriverError(
            "the kubernetes driver needs --namespace: it is where the install record lives and \
             the default for resources that name none"
                .to_owned(),
        )
    })?;
    match namespace_problem(namespace) {
        Some(problem) => Err(DriverError(format!("--namespace {problem}"))),
        None => Ok(namespace.clone()),
    }
}

/// The `--timeout` value, or ten minutes.
pub(crate) fn timeout_of(flags: &BTreeMap<String, String>) -> Result<Duration, DriverError> {
    parse_timeout(flags.get("timeout").map_or("10m", String::as_str))
        .map_err(|problem| DriverError(format!("--timeout {problem}")))
}

fn settings_of(prepared: &Prepared) -> Result<&Settings, DriverError> {
    prepared.state.downcast_ref::<Settings>().ok_or_else(|| {
        DriverError("the kubernetes driver was handed another driver's state".to_owned())
    })
}

impl Driver for KubernetesDriver {
    fn id(&self) -> &'static str {
        "kubernetes"
    }

    fn flags(&self) -> Vec<DriverFlag> {
        let mut flags = connection_flags();
        flags.extend([
            value_flag(
                "namespace",
                "Namespace for the install record and for resources that name none",
            ),
            value_flag("name", "Name of this install (default: the app's id)"),
            value_flag(
                "timeout",
                "How long to wait for workloads, e.g. 90s, 10m (default 10m)",
            ),
            switch(
                "force-conflicts",
                "Take over fields that other managers own",
            ),
        ]);
        flags
    }

    fn prepare(&self, input: &DriverInput<'_>) -> Result<Prepared, DriverError> {
        let flags = input.flags;
        // Everything that can be wrong with the flags is found before anything is contacted.
        let namespace = namespace_of(flags)?;
        let name = flags
            .get("name")
            .cloned()
            .unwrap_or_else(|| input.app.to_owned());
        if let Some(problem) = install_name_problem(&name) {
            return Err(DriverError(format!("--name {problem}")));
        }
        let timeout = timeout_of(flags)?;
        let connection = open(flags)?;
        Ok(Prepared {
            settings: vec![
                ("namespace".to_owned(), namespace.clone()),
                ("name".to_owned(), name.clone()),
            ],
            state: Box::new(Settings {
                connection,
                app: input.app.to_owned(),
                parts: input.parts.to_vec(),
                name,
                namespace,
                timeout,
                force_conflicts: flags.contains_key("force-conflicts"),
            }),
        })
    }

    fn seed(&self) -> Value {
        Value::Null
    }

    fn check_answer(&self, answer: &Value, reader: &dyn ObjectReader) -> Result<(), ContractError> {
        // A plugin with nothing to add passes on what it was given, which may be nothing yet.
        if matches!(answer, Value::Null) {
            return Ok(());
        }
        checked(answer, reader).map(drop)
    }

    fn install(
        &self,
        prepared: &Prepared,
        answer: &Value,
        reader: &dyn ObjectReader,
        plugins: &[PluginRef],
        progress: &mut dyn Progress,
    ) -> Result<Value, DriverError> {
        let settings = settings_of(prepared)?;
        if matches!(answer, Value::Null) {
            return Err(DriverError(
                "no plugin supplied anything to install".to_owned(),
            ));
        }
        let resources = checked(answer, reader).map_err(|e| DriverError(e.to_string()))?;
        let identity = InstallIdentity {
            app: &settings.app,
            name: &settings.name,
            namespace: &settings.namespace,
        };
        let applied = apply_install(
            &settings.connection,
            &resources,
            &InstallOptions {
                identity,
                parts: &settings.parts,
                force_conflicts: settings.force_conflicts,
                plugins,
                cli_version: self.cli_version,
            },
            progress,
        )
        .map_err(DriverError)?;
        for dropped in &applied.retained {
            progress.step(&format!(
                "{} is no longer part of this install and was left running; uninstall removes it",
                dropped.display()
            ));
        }
        wait_and_finish(
            &settings.connection,
            &applied,
            resources.wait_for.as_deref(),
            &identity,
            settings.timeout,
            POLL,
            progress,
        )
        .map_err(DriverError)?;

        let text = |value: &str| Value::Str(value.to_owned());
        let items = applied
            .resources
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let fields = vec![
                    (Key::Str("apiVersion".to_owned()), text(&id.api_version)),
                    (Key::Str("kind".to_owned()), text(&id.kind)),
                    (
                        Key::Str("namespace".to_owned()),
                        text(id.namespace.as_deref().unwrap_or("")),
                    ),
                    (Key::Str("name".to_owned()), text(&id.name)),
                ];
                (Key::Int(index as i64), Value::Array(fields))
            })
            .collect();
        Ok(Value::Array(items))
    }

    fn report(&self, result: &Value) -> Vec<String> {
        let Value::Array(items) = result else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|(_, item)| {
                let Value::Array(fields) = item else {
                    return None;
                };
                let text = |name: &str| {
                    fields.iter().find_map(|(key, value)| match (key, value) {
                        (Key::Str(key), Value::Str(value)) if key == name => Some(value.as_str()),
                        _ => None,
                    })
                };
                let namespace = match text("namespace")? {
                    "" => "-",
                    namespace => namespace,
                };
                Some(format!("{namespace}\t{}/{}", text("kind")?, text("name")?))
            })
            .collect()
    }

    fn uninstall_flags(&self) -> Vec<DriverFlag> {
        let mut flags = connection_flags();
        flags.extend([
            value_flag("namespace", "Namespace the install's record is in"),
            value_flag(
                "timeout",
                "How long to wait for things to be gone, e.g. 90s, 10m (default 10m)",
            ),
        ]);
        flags
    }

    fn plan_uninstall(&self, input: &UninstallInput<'_>) -> Result<UninstallPlan, DriverError> {
        let namespace = namespace_of(input.flags)?;
        if let Some(problem) = install_name_problem(input.name) {
            return Err(DriverError(format!("the install name {problem}")));
        }
        let timeout = timeout_of(input.flags)?;
        let mut connection = open(input.flags)?;
        // Nothing is being defined here: a kind the cluster does not have is simply gone.
        connection.set_discovery_wait(Duration::ZERO);
        let removal = find_install(&connection, input.name, &namespace).map_err(DriverError)?;
        Ok(UninstallPlan {
            summary: summarise(&removal),
            state: Box::new(Uninstalling {
                connection,
                removal,
                timeout,
            }),
        })
    }

    fn uninstall(
        &self,
        plan: &UninstallPlan,
        progress: &mut dyn Progress,
    ) -> Result<Vec<String>, DriverError> {
        let state = plan.state.downcast_ref::<Uninstalling>().ok_or_else(|| {
            DriverError("the kubernetes driver was handed another driver's plan".to_owned())
        })?;
        let removed = remove_install(
            &state.connection,
            &state.removal,
            state.timeout,
            POLL,
            progress,
        )
        .map_err(DriverError)?;
        for (id, why) in &removed.skipped {
            progress.step(&format!("{} was left alone: {why}", id.display()));
        }
        for (namespace, why) in &removed.kept_namespaces {
            progress.step(&format!("namespace {namespace} was kept: {why}"));
        }

        // In the order they were handled: the record's resources, last first.
        let mut lines = Vec::new();
        for recorded in state.removal.record.resources.iter().rev() {
            let id = &recorded.id;
            let outcome = if removed.deleted.contains(id) {
                "deleted"
            } else if removed.already_gone.contains(id) {
                "gone"
            } else {
                "skipped"
            };
            lines.push(format!(
                "{}\t{}/{}\t{outcome}",
                id.namespace.as_deref().unwrap_or("-"),
                id.kind,
                id.name
            ));
        }
        for namespace in &removed.namespaces {
            lines.push(format!("-\tNamespace/{namespace}\tdeleted"));
        }
        for (namespace, _) in &removed.kept_namespaces {
            lines.push(format!("-\tNamespace/{namespace}\tkept"));
        }
        Ok(lines)
    }
}

/// The answer decoded, with every wait checked to have a rule: a built-in one for the kind, or
/// a condition.
fn checked(
    answer: &Value,
    reader: &dyn ObjectReader,
) -> Result<KubernetesResources, ContractError> {
    let resources = decode_kubernetes_resources(answer, reader)?;
    for (index, wait) in resources.wait_for.iter().flatten().enumerate() {
        if wait.condition.is_none() && !KNOWN_KINDS.contains(&wait.kind.as_str()) {
            return Err(ContractError {
                contract: "KubernetesResources",
                field: format!("waitFor[{index}]"),
                problem: format!("no readiness rule for {}; give a condition", wait.kind),
            });
        }
    }
    Ok(resources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    type Props = Vec<(String, Value)>;

    #[derive(Default)]
    struct FakeReader {
        objects: RefCell<HashMap<usize, (String, Props)>>,
    }

    impl FakeReader {
        fn object(&self, class: &str, props: Vec<(&str, Value)>) -> Value {
            let rc: Rc<dyn Any> = Rc::new(0u8);
            let props = props.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
            self.objects.borrow_mut().insert(
                Rc::as_ptr(&rc) as *const () as usize,
                (class.to_owned(), props),
            );
            Value::Opaque(rc)
        }
    }

    impl ObjectReader for FakeReader {
        fn class_of(&self, value: &Value) -> Option<String> {
            let Value::Opaque(rc) = value else {
                return None;
            };
            let key = Rc::as_ptr(rc) as *const () as usize;
            self.objects.borrow().get(&key).map(|o| o.0.clone())
        }

        fn properties_of(&self, value: &Value) -> Option<Props> {
            let Value::Opaque(rc) = value else {
                return None;
            };
            let key = Rc::as_ptr(rc) as *const () as usize;
            self.objects.borrow().get(&key).map(|o| o.1.clone())
        }
    }

    fn flags(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn prepare(given: &BTreeMap<String, String>) -> Result<Prepared, DriverError> {
        KubernetesDriver::new().prepare(&DriverInput {
            app: "site",
            parts: &[],
            flags: given,
            interactive: false,
        })
    }

    /// Flags that connect without reaching anything: a server and a token file.
    fn connectable(extra: &[(&str, &str)]) -> (tempfile::NamedTempFile, BTreeMap<String, String>) {
        let token = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(token.path(), "a-token").unwrap();
        let mut given = flags(&[
            ("server", "https://cluster.test:6443"),
            ("token-file", token.path().to_str().unwrap()),
        ]);
        given.extend(flags(extra));
        (token, given)
    }

    #[test]
    fn describes_itself() {
        let driver = KubernetesDriver::new();
        assert_eq!(driver.id(), "kubernetes");
        assert_eq!(driver.seed(), Value::Null);
        let described: Vec<(&str, bool)> = driver
            .flags()
            .iter()
            .map(|flag| (flag.name, flag.takes_value))
            .collect();
        assert_eq!(
            described,
            vec![
                ("server", true),
                ("token-file", true),
                ("ca-cert", true),
                ("insecure-skip-tls-verify", false),
                ("context", true),
                ("namespace", true),
                ("name", true),
                ("timeout", true),
                ("force-conflicts", false),
            ]
        );
    }

    #[test]
    fn namespace_is_required() {
        let (_token, given) = connectable(&[]);
        let error = prepare(&given).err().expect("no namespace was given");
        assert!(error.to_string().contains("--namespace"), "{error}");
        let (_token, given) = connectable(&[("namespace", "Not_A_Label")]);
        assert!(prepare(&given).is_err());
    }

    #[test]
    fn name_defaults_to_the_app() {
        let (_token, given) = connectable(&[("namespace", "shop")]);
        let prepared = prepare(&given).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            prepared.settings,
            vec![
                ("namespace".to_owned(), "shop".to_owned()),
                ("name".to_owned(), "site".to_owned()),
            ]
        );
        let (_token, given) = connectable(&[("namespace", "shop"), ("name", "prod")]);
        assert_eq!(
            prepare(&given).unwrap_or_else(|e| panic!("{e}")).settings[1].1,
            "prod"
        );
        let (_token, given) = connectable(&[("namespace", "shop"), ("name", "Not A Name")]);
        assert!(prepare(&given).is_err());
    }

    #[test]
    fn settings_never_include_server_or_token() {
        let (_token, given) = connectable(&[("namespace", "shop")]);
        let prepared = prepare(&given).unwrap_or_else(|e| panic!("{e}"));
        let shared = format!("{:?}", prepared.settings);
        assert!(
            !shared.contains("cluster.test") && !shared.contains("a-token"),
            "{shared}"
        );
        let names: Vec<&str> = prepared
            .settings
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(names, vec!["namespace", "name"]);
    }

    #[test]
    fn bad_timeout_is_reported_before_connecting() {
        // No server, token or kubeconfig is needed to learn that the timeout is wrong.
        let given = flags(&[("namespace", "shop"), ("timeout", "soon")]);
        let error = prepare(&given).err().expect("the timeout is wrong");
        assert!(error.to_string().contains("--timeout"), "{error}");
    }

    #[test]
    fn null_answer_passes_the_check_and_fails_the_install() {
        let reader = FakeReader::default();
        let driver = KubernetesDriver::new();
        assert!(driver.check_answer(&Value::Null, &reader).is_ok());
        let (_token, given) = connectable(&[("namespace", "shop")]);
        let prepared = prepare(&given).unwrap_or_else(|e| panic!("{e}"));
        struct Quiet;
        impl Progress for Quiet {
            fn step(&mut self, _message: &str) {}
        }
        let error = driver
            .install(&prepared, &Value::Null, &reader, &[], &mut Quiet)
            .unwrap_err();
        assert_eq!(error.to_string(), "no plugin supplied anything to install");
    }

    #[test]
    fn wait_without_a_rule_is_refused() {
        let reader = FakeReader::default();
        let list = |items: Vec<Value>| {
            Value::Array(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| (Key::Int(i as i64), v))
                    .collect(),
            )
        };
        let text = |value: &str| Value::Str(value.to_owned());
        let widget = Value::Array(vec![
            (Key::Str("apiVersion".to_owned()), text("example.test/v1")),
            (Key::Str("kind".to_owned()), text("Widget")),
            (
                Key::Str("metadata".to_owned()),
                Value::Array(vec![(Key::Str("name".to_owned()), text("w"))]),
            ),
        ]);
        let wait = |condition: Value| {
            reader.object(
                "SpeedySpec\\Cli\\Contracts\\WaitFor",
                vec![
                    ("kind", text("Widget")),
                    ("name", text("w")),
                    ("namespace", Value::Null),
                    ("condition", condition),
                ],
            )
        };
        let answer = |wait: Value| {
            reader.object(
                "SpeedySpec\\Cli\\Contracts\\KubernetesResources",
                vec![
                    ("resources", list(vec![widget.clone()])),
                    ("waitFor", list(vec![wait])),
                ],
            )
        };
        let driver = KubernetesDriver::new();
        let error = driver
            .check_answer(&answer(wait(Value::Null)), &reader)
            .unwrap_err();
        assert_eq!(error.field, "waitFor[0]");
        assert!(error.problem.contains("Widget"), "{}", error.problem);
        assert!(
            driver
                .check_answer(&answer(wait(text("Available"))), &reader)
                .is_ok()
        );
    }
}
