//! The Kubernetes driver's contract: the `KubernetesResources` a plugin returns, decoded into
//! JSON manifests and checked as far as it can be without a cluster.

use serde_json::Value as Json;
use speedyspec_hooks::{Key, Value};

use crate::contracts::{ContractError, ObjectReader};

const RESOURCES_CLASS: &str = "SpeedySpec\\Cli\\Contracts\\KubernetesResources";
const WAIT_FOR_CLASS: &str = "SpeedySpec\\Cli\\Contracts\\WaitFor";

/// A resource the install waits on before it is called done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitFor {
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
    /// A status condition that must be `True`; needed for kinds with no built-in rule.
    pub condition: Option<String>,
}

/// What a plugin wants in the cluster.
#[derive(Clone, Debug, PartialEq)]
pub struct KubernetesResources {
    /// Complete manifests, in the order to apply them. Each is a JSON object.
    pub resources: Vec<Json>,
    /// `None`: wait on the default set. `Some(empty)`: wait on nothing.
    pub wait_for: Option<Vec<WaitFor>>,
}

/// Decodes and checks one `KubernetesResources` object. Fields are reported as paths such as
/// `resources[2].metadata.name`.
pub fn decode_kubernetes_resources(
    value: &Value,
    reader: &dyn ObjectReader,
) -> Result<KubernetesResources, ContractError> {
    let properties = object(value, RESOURCES_CLASS, reader).ok_or_else(|| {
        error(
            "",
            &format!("expected an object of class {RESOURCES_CLASS}"),
        )
    })?;

    let items = property(&properties, "resources")
        .and_then(as_list)
        .ok_or_else(|| error("resources", LIST))?;
    let mut resources = Vec::with_capacity(items.len());
    let mut seen: Vec<Identity> = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        let path = format!("resources[{index}]");
        let manifest = value_to_json(item, reader).map_err(|problem| error(&path, &problem))?;
        let identity = identity(&manifest, &path)?;
        if seen.contains(&identity) {
            return Err(error(
                &path,
                "another resource of this kind, namespace and name is already in the list",
            ));
        }
        seen.push(identity);
        resources.push(manifest);
    }

    let wait_for = match property(&properties, "waitFor") {
        None | Some(Value::Null) => None,
        Some(given) => {
            let entries = as_list(given).ok_or_else(|| error("waitFor", LIST))?;
            let mut waits = Vec::with_capacity(entries.len());
            for (index, entry) in entries.into_iter().enumerate() {
                let path = format!("waitFor[{index}]");
                let wait = wait_for(entry, reader, &path)?;
                let named = seen.iter().any(|resource| {
                    resource.kind == wait.kind
                        && resource.name == wait.name
                        && match (&wait.namespace, &resource.namespace) {
                            (Some(wanted), Some(has)) => wanted == has,
                            // A resource without a namespace takes the install's, whatever it is.
                            _ => true,
                        }
                });
                if !named {
                    return Err(error(
                        &path,
                        &format!(
                            "no {} named \"{}\" is being installed",
                            wait.kind, wait.name
                        ),
                    ));
                }
                waits.push(wait);
            }
            Some(waits)
        }
    };

    Ok(KubernetesResources {
        resources,
        wait_for,
    })
}

const LIST: &str = "expected a list (an array keyed 0, 1, 2, ...)";

fn error(field: &str, problem: &str) -> ContractError {
    ContractError {
        contract: "KubernetesResources",
        field: field.to_owned(),
        problem: problem.to_owned(),
    }
}

/// What makes two resources the same thing in a cluster.
#[derive(PartialEq, Eq)]
struct Identity {
    group: String,
    kind: String,
    namespace: Option<String>,
    name: String,
}

/// Checks that `manifest` says what it is, and returns that.
fn identity(manifest: &Json, path: &str) -> Result<Identity, ContractError> {
    let Some(fields) = manifest.as_object() else {
        return Err(error(
            path,
            "expected a resource: an array with apiVersion, kind and metadata",
        ));
    };
    let text = |holder: &serde_json::Map<String, Json>, key: &str, at: &str| match holder
        .get(key)
        .and_then(Json::as_str)
    {
        Some(text) if !text.is_empty() => Ok(text.to_owned()),
        _ => Err(error(at, "expected a non-empty string")),
    };
    let api_version = text(fields, "apiVersion", &format!("{path}.apiVersion"))?;
    let kind = text(fields, "kind", &format!("{path}.kind"))?;
    let metadata = fields
        .get("metadata")
        .and_then(Json::as_object)
        .ok_or_else(|| error(&format!("{path}.metadata"), "expected an array with a name"))?;
    let name = text(metadata, "name", &format!("{path}.metadata.name"))?;
    let namespace = match metadata.get("namespace") {
        None | Some(Json::Null) => None,
        Some(_) => Some(text(
            metadata,
            "namespace",
            &format!("{path}.metadata.namespace"),
        )?),
    };
    let group = match api_version.split_once('/') {
        Some((group, _)) => group.to_owned(),
        None => String::new(),
    };
    Ok(Identity {
        group,
        kind,
        namespace,
        name,
    })
}

fn wait_for(
    value: &Value,
    reader: &dyn ObjectReader,
    path: &str,
) -> Result<WaitFor, ContractError> {
    let properties = object(value, WAIT_FOR_CLASS, reader).ok_or_else(|| {
        error(
            path,
            &format!("expected an object of class {WAIT_FOR_CLASS}"),
        )
    })?;
    let required = |name: &str| match property(&properties, name) {
        Some(Value::Str(text)) if !text.is_empty() => Ok(text.clone()),
        _ => Err(error(
            &format!("{path}.{name}"),
            "expected a non-empty string",
        )),
    };
    let optional = |name: &str| match property(&properties, name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Str(text)) if !text.is_empty() => Ok(Some(text.clone())),
        _ => Err(error(
            &format!("{path}.{name}"),
            "expected null or a non-empty string",
        )),
    };
    Ok(WaitFor {
        kind: required("kind")?,
        name: required("name")?,
        namespace: optional("namespace")?,
        condition: optional("condition")?,
    })
}

/// The properties of `value` if it is an object of exactly `class`.
fn object(value: &Value, class: &str, reader: &dyn ObjectReader) -> Option<Vec<(String, Value)>> {
    (reader.class_of(value).as_deref() == Some(class))
        .then(|| reader.properties_of(value))
        .flatten()
}

fn property<'p>(properties: &'p [(String, Value)], name: &str) -> Option<&'p Value> {
    properties
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

/// The items of a PHP list: an array keyed 0, 1, 2, ... in order (the empty array included).
fn as_list(value: &Value) -> Option<Vec<&Value>> {
    let Value::Array(items) = value else {
        return None;
    };
    items
        .iter()
        .enumerate()
        .all(|(index, (key, _))| *key == Key::Int(index as i64))
        .then(|| items.iter().map(|(_, item)| item).collect())
}

/// A PHP value as JSON, by the rule `json_encode` follows: a list (keys 0, 1, 2, ... in order,
/// the empty array included) is an array; any other array is an object; a `stdClass` is an
/// object of its properties, so `new stdClass` is `{}`.
pub fn value_to_json(value: &Value, reader: &dyn ObjectReader) -> Result<Json, String> {
    match value {
        Value::Null => Ok(Json::Null),
        Value::Bool(flag) => Ok(Json::Bool(*flag)),
        Value::Int(number) => Ok(Json::from(*number)),
        Value::Float(number) => serde_json::Number::from_f64(*number)
            .map(Json::Number)
            .ok_or_else(|| "a number that is not finite cannot be sent to a cluster".to_owned()),
        Value::Str(text) => Ok(Json::String(text.clone())),
        Value::Bytes(_) => Err("a string that is not valid UTF-8".to_owned()),
        Value::Array(items) => {
            if let Some(list) = as_list(value) {
                return list
                    .into_iter()
                    .map(|item| value_to_json(item, reader))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Json::Array);
            }
            let mut fields = serde_json::Map::with_capacity(items.len());
            for (key, item) in items {
                let key = match key {
                    Key::Int(number) => number.to_string(),
                    Key::Str(text) => text.clone(),
                };
                fields.insert(key, value_to_json(item, reader)?);
            }
            Ok(Json::Object(fields))
        }
        Value::Opaque(_) => match reader.class_of(value).as_deref() {
            Some("stdClass") => {
                let properties = reader.properties_of(value).unwrap_or_default();
                let mut fields = serde_json::Map::with_capacity(properties.len());
                for (name, item) in &properties {
                    fields.insert(name.clone(), value_to_json(item, reader)?);
                }
                Ok(Json::Object(fields))
            }
            Some(class) => Err(format!(
                "an object of class {class} cannot be part of a resource (use arrays, and \
                 `new stdClass` for an empty object)"
            )),
            None => Err("a value that cannot be part of a resource".to_owned()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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

        fn std(&self, props: Vec<(&str, Value)>) -> Value {
            self.object("stdClass", props)
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

    fn s(text: &str) -> Value {
        Value::Str(text.to_owned())
    }

    fn list(items: Vec<Value>) -> Value {
        Value::Array(
            items
                .into_iter()
                .enumerate()
                .map(|(i, v)| (Key::Int(i as i64), v))
                .collect(),
        )
    }

    fn map(pairs: Vec<(&str, Value)>) -> Value {
        Value::Array(
            pairs
                .into_iter()
                .map(|(k, v)| (Key::Str(k.to_owned()), v))
                .collect(),
        )
    }

    /// A manifest as the PHP array a plugin would write.
    fn manifest(api_version: &str, kind: &str, namespace: Option<&str>, name: &str) -> Value {
        let mut metadata = vec![("name", s(name))];
        if let Some(namespace) = namespace {
            metadata.push(("namespace", s(namespace)));
        }
        map(vec![
            ("apiVersion", s(api_version)),
            ("kind", s(kind)),
            ("metadata", map(metadata)),
        ])
    }

    fn resources(reader: &FakeReader, items: Vec<Value>, wait_for: Value) -> Value {
        reader.object(
            RESOURCES_CLASS,
            vec![("resources", list(items)), ("waitFor", wait_for)],
        )
    }

    fn wait(reader: &FakeReader, kind: &str, name: &str, namespace: Option<&str>) -> Value {
        reader.object(
            WAIT_FOR_CLASS,
            vec![
                ("kind", s(kind)),
                ("name", s(name)),
                ("namespace", namespace.map_or(Value::Null, s)),
                ("condition", Value::Null),
            ],
        )
    }

    fn field(result: Result<KubernetesResources, ContractError>) -> String {
        let error = result.expect_err("expected a contract error");
        assert_eq!(error.contract, "KubernetesResources");
        error.field
    }

    #[test]
    fn json_conversion_follows_json_encode() {
        let reader = FakeReader::default();
        let json = |value: Value| value_to_json(&value, &reader).unwrap();
        assert_eq!(json(list(vec![])), json!([]));
        assert_eq!(json(list(vec![Value::Int(1), s("a")])), json!([1, "a"]));
        assert_eq!(json(map(vec![("k", Value::Int(1))])), json!({"k": 1}));
        let sparse = Value::Array(vec![(Key::Int(1), s("a"))]);
        assert_eq!(json(sparse), json!({"1": "a"}));
        assert_eq!(json(reader.std(vec![])), json!({}));
        let nested = reader.std(vec![("a", list(vec![reader.std(vec![])]))]);
        assert_eq!(json(nested), json!({"a": [{}]}));
        let mixed = map(vec![
            ("n", Value::Null),
            ("b", Value::Bool(true)),
            ("f", Value::Float(1.5)),
            (
                "deep",
                map(vec![("list", list(vec![map(vec![("x", Value::Int(2))])]))]),
            ),
        ]);
        assert_eq!(
            json(mixed),
            json!({"n": null, "b": true, "f": 1.5, "deep": {"list": [{"x": 2}]}})
        );
    }

    #[test]
    fn json_conversion_refuses_other_objects_and_bad_scalars() {
        let reader = FakeReader::default();
        let closure = reader.object("Closure", vec![]);
        let error = value_to_json(&closure, &reader).unwrap_err();
        assert!(error.contains("Closure"), "{error}");
        assert!(value_to_json(&Value::Bytes(vec![0xff]), &reader).is_err());
        assert!(value_to_json(&Value::Float(f64::NAN), &reader).is_err());
        assert!(value_to_json(&Value::Float(f64::INFINITY), &reader).is_err());
        // An opaque value nobody can describe.
        let unknown = Value::Opaque(Rc::new(0u8));
        assert!(value_to_json(&unknown, &reader).is_err());
    }

    #[test]
    fn decodes_resources_and_default_wait() {
        let reader = FakeReader::default();
        let value = resources(
            &reader,
            vec![
                manifest("v1", "ConfigMap", Some("shop"), "settings"),
                manifest("apps/v1", "Deployment", None, "web"),
            ],
            Value::Null,
        );
        let decoded = decode_kubernetes_resources(&value, &reader).unwrap();
        assert_eq!(decoded.wait_for, None);
        assert_eq!(decoded.resources.len(), 2);
        assert_eq!(decoded.resources[0]["metadata"]["namespace"], json!("shop"));
        assert_eq!(decoded.resources[1]["kind"], json!("Deployment"));
    }

    #[test]
    fn decodes_an_explicit_empty_wait() {
        let reader = FakeReader::default();
        let value = resources(
            &reader,
            vec![manifest("v1", "ConfigMap", None, "a")],
            list(vec![]),
        );
        let decoded = decode_kubernetes_resources(&value, &reader).unwrap();
        assert_eq!(decoded.wait_for, Some(vec![]));
    }

    #[test]
    fn empty_resources_are_valid_here() {
        let reader = FakeReader::default();
        let value = resources(&reader, vec![], Value::Null);
        assert!(
            decode_kubernetes_resources(&value, &reader)
                .unwrap()
                .resources
                .is_empty()
        );
    }

    #[test]
    fn rejects_wrong_class() {
        let reader = FakeReader::default();
        let error = decode_kubernetes_resources(&reader.std(vec![]), &reader).unwrap_err();
        assert_eq!(error.field, "");
        assert!(error.problem.contains(RESOURCES_CLASS), "{}", error.problem);
        assert_eq!(
            field(decode_kubernetes_resources(&Value::Null, &reader)),
            ""
        );
    }

    #[test]
    fn rejects_resource_without_identity() {
        let reader = FakeReader::default();
        let name_only = map(vec![("name", s("a"))]);
        let cases: Vec<(Value, &str)> = vec![
            (
                map(vec![
                    ("kind", s("ConfigMap")),
                    ("metadata", name_only.clone()),
                ]),
                "resources[0].apiVersion",
            ),
            (
                map(vec![
                    ("apiVersion", s("v1")),
                    ("kind", s("")),
                    ("metadata", name_only),
                ]),
                "resources[0].kind",
            ),
            (
                map(vec![("apiVersion", s("v1")), ("kind", s("ConfigMap"))]),
                "resources[0].metadata",
            ),
            (
                map(vec![
                    ("apiVersion", s("v1")),
                    ("kind", s("ConfigMap")),
                    ("metadata", map(vec![("labels", list(vec![]))])),
                ]),
                "resources[0].metadata.name",
            ),
            (
                map(vec![
                    ("apiVersion", s("v1")),
                    ("kind", s("ConfigMap")),
                    (
                        "metadata",
                        map(vec![("name", s("a")), ("namespace", s(""))]),
                    ),
                ]),
                "resources[0].metadata.namespace",
            ),
            (s("not a manifest"), "resources[0]"),
            (list(vec![]), "resources[0]"),
        ];
        for (item, expected) in cases {
            let value = resources(&reader, vec![item], Value::Null);
            assert_eq!(
                field(decode_kubernetes_resources(&value, &reader)),
                expected
            );
        }
    }

    #[test]
    fn a_bad_value_inside_a_resource_is_located() {
        let reader = FakeReader::default();
        let mut item = manifest("v1", "ConfigMap", None, "a");
        if let Value::Array(fields) = &mut item {
            fields.push((
                Key::Str("data".to_owned()),
                reader.object("Closure", vec![]),
            ));
        }
        let value = resources(&reader, vec![item], Value::Null);
        assert_eq!(
            field(decode_kubernetes_resources(&value, &reader)),
            "resources[0]"
        );
    }

    #[test]
    fn rejects_duplicate_resources() {
        let reader = FakeReader::default();
        let value = resources(
            &reader,
            vec![
                manifest("apps/v1", "Deployment", Some("shop"), "web"),
                manifest("apps/v1beta1", "Deployment", Some("shop"), "web"),
            ],
            Value::Null,
        );
        assert_eq!(
            field(decode_kubernetes_resources(&value, &reader)),
            "resources[1]"
        );
    }

    #[test]
    fn same_name_in_two_namespaces_is_not_a_duplicate() {
        let reader = FakeReader::default();
        let value = resources(
            &reader,
            vec![
                manifest("v1", "ConfigMap", Some("a"), "settings"),
                manifest("v1", "ConfigMap", Some("b"), "settings"),
                manifest("v1", "Secret", Some("a"), "settings"),
            ],
            Value::Null,
        );
        assert!(decode_kubernetes_resources(&value, &reader).is_ok());
    }

    #[test]
    fn wait_for_must_name_an_installed_resource() {
        let reader = FakeReader::default();
        let value = resources(
            &reader,
            vec![manifest("apps/v1", "Deployment", None, "web")],
            list(vec![wait(&reader, "Deployment", "worker", None)]),
        );
        assert_eq!(
            field(decode_kubernetes_resources(&value, &reader)),
            "waitFor[0]"
        );

        let value = resources(
            &reader,
            vec![manifest("apps/v1", "Deployment", None, "web")],
            list(vec![reader.std(vec![])]),
        );
        assert_eq!(
            field(decode_kubernetes_resources(&value, &reader)),
            "waitFor[0]"
        );
    }

    #[test]
    fn wait_for_namespace_narrows_the_match() {
        let reader = FakeReader::default();
        let items = || {
            vec![
                manifest("apps/v1", "Deployment", Some("a"), "web"),
                manifest("apps/v1", "Deployment", Some("b"), "web"),
            ]
        };
        let value = resources(
            &reader,
            items(),
            list(vec![wait(&reader, "Deployment", "web", Some("b"))]),
        );
        let decoded = decode_kubernetes_resources(&value, &reader).unwrap();
        assert_eq!(decoded.wait_for.unwrap()[0].namespace.as_deref(), Some("b"));

        let value = resources(
            &reader,
            items(),
            list(vec![wait(&reader, "Deployment", "web", Some("c"))]),
        );
        assert_eq!(
            field(decode_kubernetes_resources(&value, &reader)),
            "waitFor[0]"
        );
    }
}
