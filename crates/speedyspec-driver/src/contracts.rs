//! Decoding and validation of the contract objects plugins return.
//!
//! A PHP object arrives as `Value::Opaque`; this module looks inside only
//! through [`ObjectReader`], so it never depends on the PHP runtime.

use std::fmt;

use speedyspec_hooks::{Key, Value};

const NS: &str = "SpeedySpec\\Cli\\Contracts\\";

/// Reads class and properties of a PHP object held in a `Value::Opaque`.
/// Implemented on top of the PHP runtime.
pub trait ObjectReader {
    /// Fully qualified class name, or `None` if the value is not an object.
    fn class_of(&self, value: &Value) -> Option<String>;
    /// Property names and values, or `None` if the value is not an object.
    fn properties_of(&self, value: &Value) -> Option<Vec<(String, Value)>>;
}

/// A contract value that is wrong, with the path of the offending field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractError {
    /// Short name of the top-level contract being decoded: `App` or `Command`.
    pub contract: &'static str,
    /// Path such as `[1].parts[0].id`; empty when the top-level value is wrong.
    pub field: String,
    pub problem: String,
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.field.is_empty() {
            f.write_str(&self.problem)
        } else {
            write!(f, "{}: {}", self.field, self.problem)
        }
    }
}

impl std::error::Error for ContractError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPart {
    pub id: String,
    pub label: String,
    pub default: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    pub id: String,
    pub label: String,
    pub parts: Vec<AppPart>,
    pub drivers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandArgument {
    pub name: String,
    pub description: String,
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOption {
    pub name: String,
    pub description: String,
    pub takes_value: bool,
    pub default: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Command {
    pub name: String,
    pub description: String,
    /// The closure object, carried as it arrived (same `Rc`).
    pub handler: Value,
    pub arguments: Vec<CommandArgument>,
    pub options: Vec<CommandOption>,
}

/// Decodes the value a plugin returned for apps into validated [`App`]s: a list of
/// [`decode_app`] items with unique ids. Fields are reported as `[i].field`.
pub fn decode_apps(value: &Value, reader: &dyn ObjectReader) -> Result<Vec<App>, ContractError> {
    let d = Decoder {
        reader,
        contract: "App",
    };
    let items = d.list(value, "")?;
    let mut ids = Vec::new();
    items
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            let path = format!("[{i}]");
            let app = d.app(item, &path)?;
            d.unique(&mut ids, &app.id, &path, "id")?;
            Ok(app)
        })
        .collect()
}

/// Decodes one `App` object. Fields are reported relative to it (`id`, `parts[0].label`); an
/// empty field means the value itself is wrong.
pub fn decode_app(value: &Value, reader: &dyn ObjectReader) -> Result<App, ContractError> {
    Decoder {
        reader,
        contract: "App",
    }
    .app(value, "")
}

/// Decodes the value a plugin returned for commands into validated [`Command`]s: a list of
/// [`decode_command`] items with unique names. Fields are reported as `[i].field`.
pub fn decode_commands(
    value: &Value,
    reader: &dyn ObjectReader,
) -> Result<Vec<Command>, ContractError> {
    let d = Decoder {
        reader,
        contract: "Command",
    };
    let items = d.list(value, "")?;
    let mut names = Vec::new();
    items
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            let path = format!("[{i}]");
            let command = d.command(item, &path)?;
            d.unique(&mut names, &command.name, &path, "name")?;
            Ok(command)
        })
        .collect()
}

/// Decodes one `Command` object. Fields are reported relative to it (`name`,
/// `options[0].default`); an empty field means the value itself is wrong.
pub fn decode_command(value: &Value, reader: &dyn ObjectReader) -> Result<Command, ContractError> {
    Decoder {
        reader,
        contract: "Command",
    }
    .command(value, "")
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{path}.{name}")
    }
}

/// `^[a-z][a-z0-9-]*$`
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-'))
}

type Props = Vec<(String, Value)>;

struct Decoder<'a> {
    reader: &'a dyn ObjectReader,
    contract: &'static str,
}

impl Decoder<'_> {
    fn err(&self, field: &str, problem: &str) -> ContractError {
        ContractError {
            contract: self.contract,
            field: field.to_string(),
            problem: problem.to_string(),
        }
    }

    /// A PHP list: an array keyed 0, 1, 2, ... in order.
    fn list<'v>(&self, value: &'v Value, path: &str) -> Result<Vec<&'v Value>, ContractError> {
        if let Value::Array(items) = value {
            let is_list = items
                .iter()
                .enumerate()
                .all(|(i, (k, _))| *k == Key::Int(i as i64));
            if is_list {
                return Ok(items.iter().map(|(_, v)| v).collect());
            }
        }
        Err(self.err(path, "expected a list (an array keyed 0, 1, 2, ...)"))
    }

    fn app(&self, value: &Value, path: &str) -> Result<App, ContractError> {
        let props = self.object(value, "App", path)?;
        Ok(App {
            id: self.id(&props, "id", path)?,
            label: self.non_empty(&props, "label", path)?,
            parts: self.parts(&props, path)?,
            drivers: self.drivers(&props, path)?,
        })
    }

    fn command(&self, value: &Value, path: &str) -> Result<Command, ContractError> {
        let props = self.object(value, "Command", path)?;
        let name = self.id(&props, "name", path)?;
        let description = self.string(&props, "description", path)?;
        let handler = match self.get(&props, "handler", path)? {
            v @ Value::Opaque(_) => v.clone(),
            _ => return Err(self.err(&join(path, "handler"), "expected a closure object")),
        };
        Ok(Command {
            name,
            description,
            handler,
            arguments: self.arguments(&props, path)?,
            options: self.options(&props, path)?,
        })
    }

    fn object(&self, value: &Value, class: &str, path: &str) -> Result<Props, ContractError> {
        let full = format!("{NS}{class}");
        let wrong = || self.err(path, &format!("expected an object of class {full}"));
        if self.reader.class_of(value).as_deref() != Some(full.as_str()) {
            return Err(wrong());
        }
        self.reader.properties_of(value).ok_or_else(wrong)
    }

    fn get<'p>(
        &self,
        props: &'p Props,
        name: &str,
        path: &str,
    ) -> Result<&'p Value, ContractError> {
        props
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
            .ok_or_else(|| self.err(&join(path, name), "property is missing"))
    }

    fn string(&self, props: &Props, name: &str, path: &str) -> Result<String, ContractError> {
        match self.get(props, name, path)? {
            Value::Str(s) => Ok(s.clone()),
            _ => Err(self.err(&join(path, name), "expected a string")),
        }
    }

    fn boolean(&self, props: &Props, name: &str, path: &str) -> Result<bool, ContractError> {
        match self.get(props, name, path)? {
            Value::Bool(b) => Ok(*b),
            _ => Err(self.err(&join(path, name), "expected a bool")),
        }
    }

    fn non_empty(&self, props: &Props, name: &str, path: &str) -> Result<String, ContractError> {
        let s = self.string(props, name, path)?;
        if s.is_empty() {
            return Err(self.err(&join(path, name), "must not be empty"));
        }
        Ok(s)
    }

    fn id(&self, props: &Props, name: &str, path: &str) -> Result<String, ContractError> {
        let s = self.string(props, name, path)?;
        if !is_identifier(&s) {
            return Err(self.err(
                &join(path, name),
                "must match ^[a-z][a-z0-9-]*$ (lowercase letters, digits and hyphens, starting with a letter)",
            ));
        }
        Ok(s)
    }

    fn unique(
        &self,
        seen: &mut Vec<String>,
        id: &str,
        path: &str,
        name: &str,
    ) -> Result<(), ContractError> {
        if seen.iter().any(|s| s == id) {
            return Err(self.err(&join(path, name), &format!("duplicate {name} \"{id}\"")));
        }
        seen.push(id.to_string());
        Ok(())
    }

    fn parts(&self, props: &Props, path: &str) -> Result<Vec<AppPart>, ContractError> {
        let field = join(path, "parts");
        let items = self.list(self.get(props, "parts", path)?, &field)?;
        let mut ids = Vec::new();
        items
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                let p = format!("{field}[{i}]");
                let o = self.object(item, "AppPart", &p)?;
                let id = self.id(&o, "id", &p)?;
                self.unique(&mut ids, &id, &p, "id")?;
                Ok(AppPart {
                    id,
                    label: self.non_empty(&o, "label", &p)?,
                    default: self.boolean(&o, "default", &p)?,
                })
            })
            .collect()
    }

    fn drivers(&self, props: &Props, path: &str) -> Result<Vec<String>, ContractError> {
        let field = join(path, "drivers");
        let items = self.list(self.get(props, "drivers", path)?, &field)?;
        let mut seen: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for (i, item) in items.into_iter().enumerate() {
            let p = format!("{field}[{i}]");
            let Value::Str(s) = item else {
                return Err(self.err(&p, "expected a string"));
            };
            if !is_identifier(s) {
                return Err(self.err(&p, "must match ^[a-z][a-z0-9-]*$"));
            }
            if seen.contains(s) {
                return Err(self.err(&p, &format!("duplicate driver \"{s}\"")));
            }
            seen.push(s.clone());
            out.push(s.clone());
        }
        Ok(out)
    }

    fn arguments(&self, props: &Props, path: &str) -> Result<Vec<CommandArgument>, ContractError> {
        let field = join(path, "arguments");
        let items = self.list(self.get(props, "arguments", path)?, &field)?;
        let mut names = Vec::new();
        items
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                let p = format!("{field}[{i}]");
                let o = self.object(item, "CommandArgument", &p)?;
                let name = self.id(&o, "name", &p)?;
                self.unique(&mut names, &name, &p, "name")?;
                Ok(CommandArgument {
                    name,
                    description: self.string(&o, "description", &p)?,
                    required: self.boolean(&o, "required", &p)?,
                })
            })
            .collect()
    }

    fn options(&self, props: &Props, path: &str) -> Result<Vec<CommandOption>, ContractError> {
        let field = join(path, "options");
        let items = self.list(self.get(props, "options", path)?, &field)?;
        let mut names = Vec::new();
        items
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                let p = format!("{field}[{i}]");
                let o = self.object(item, "CommandOption", &p)?;
                let name = self.id(&o, "name", &p)?;
                self.unique(&mut names, &name, &p, "name")?;
                let default = match self.get(&o, "default", &p)? {
                    Value::Null => None,
                    Value::Str(s) => Some(s.clone()),
                    _ => return Err(self.err(&join(&p, "default"), "expected a string or null")),
                };
                Ok(CommandOption {
                    name,
                    description: self.string(&o, "description", &p)?,
                    takes_value: self.boolean(&o, "takesValue", &p)?,
                    default,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use speedyspec_hooks::{Key, Value};
    use std::any::Any;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    const APP: &str = "SpeedySpec\\Cli\\Contracts\\App";
    const PART: &str = "SpeedySpec\\Cli\\Contracts\\AppPart";
    const COMMAND: &str = "SpeedySpec\\Cli\\Contracts\\Command";

    type Props = Vec<(String, Value)>;

    #[derive(Default)]
    struct FakeReader {
        objects: RefCell<HashMap<usize, (String, Props)>>,
    }

    fn addr(rc: &Rc<dyn Any>) -> usize {
        Rc::as_ptr(rc) as *const () as usize
    }

    impl FakeReader {
        fn object(&self, class: &str, props: Vec<(&str, Value)>) -> Value {
            let rc: Rc<dyn Any> = Rc::new(0u8);
            let props = props.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
            self.objects
                .borrow_mut()
                .insert(addr(&rc), (class.to_string(), props));
            Value::Opaque(rc)
        }
    }

    impl ObjectReader for FakeReader {
        fn class_of(&self, value: &Value) -> Option<String> {
            let Value::Opaque(rc) = value else {
                return None;
            };
            self.objects.borrow().get(&addr(rc)).map(|o| o.0.clone())
        }
        fn properties_of(&self, value: &Value) -> Option<Vec<(String, Value)>> {
            let Value::Opaque(rc) = value else {
                return None;
            };
            self.objects.borrow().get(&addr(rc)).map(|o| o.1.clone())
        }
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

    fn s(v: &str) -> Value {
        Value::Str(v.to_string())
    }

    fn part(r: &FakeReader, id: &str, label: &str) -> Value {
        r.object(
            PART,
            vec![
                ("id", s(id)),
                ("label", s(label)),
                ("default", Value::Bool(true)),
            ],
        )
    }

    fn app(r: &FakeReader, id: &str, parts: Vec<Value>, drivers: Vec<&str>) -> Value {
        r.object(
            APP,
            vec![
                ("id", s(id)),
                ("label", s("WordPress")),
                ("parts", list(parts)),
                ("drivers", list(drivers.into_iter().map(s).collect())),
            ],
        )
    }

    #[test]
    fn decodes_an_app_with_parts_and_drivers() {
        let r = FakeReader::default();
        let v = list(vec![app(
            &r,
            "wp",
            vec![part(&r, "core", "Core")],
            vec!["zip", "k8s"],
        )]);
        let apps = decode_apps(&v, &r).unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].id, "wp");
        assert_eq!(apps[0].label, "WordPress");
        assert_eq!(apps[0].parts.len(), 1);
        assert_eq!(apps[0].parts[0].id, "core");
        assert!(apps[0].parts[0].default);
        assert_eq!(apps[0].drivers, vec!["zip", "k8s"]);
    }

    #[test]
    fn rejects_non_list() {
        let r = FakeReader::default();
        let e = decode_apps(&Value::Null, &r).unwrap_err();
        assert_eq!(e.contract, "App");
        assert_eq!(e.field, "");
        assert!(e.problem.contains("list"), "{}", e.problem);
        let e = decode_commands(&Value::Null, &r).unwrap_err();
        assert_eq!(e.contract, "Command");
        // A keyed array is not a list either.
        let keyed = Value::Array(vec![(Key::Str("a".into()), Value::Null)]);
        assert!(
            decode_apps(&keyed, &r)
                .unwrap_err()
                .problem
                .contains("list")
        );
        // An empty array is a valid empty list.
        assert!(decode_apps(&list(vec![]), &r).unwrap().is_empty());
    }

    #[test]
    fn rejects_wrong_class() {
        let r = FakeReader::default();
        let v = list(vec![r.object("stdClass", vec![])]);
        let e = decode_apps(&v, &r).unwrap_err();
        assert_eq!(e.field, "[0]");
        assert!(e.problem.contains(APP), "{}", e.problem);
    }

    #[test]
    fn rejects_bad_id() {
        let r = FakeReader::default();
        let v = list(vec![app(&r, "Word Press", vec![], vec![])]);
        let e = decode_apps(&v, &r).unwrap_err();
        assert_eq!(e.field, "[0].id");
        assert!(e.problem.contains("a-z"), "{}", e.problem);
    }

    #[test]
    fn rejects_duplicate_app_ids() {
        let r = FakeReader::default();
        let v = list(vec![
            app(&r, "wp", vec![], vec![]),
            app(&r, "wp", vec![], vec![]),
        ]);
        let e = decode_apps(&v, &r).unwrap_err();
        assert_eq!(e.field, "[1].id");
        assert!(e.problem.contains("duplicate"), "{}", e.problem);
    }

    #[test]
    fn rejects_nested_part_error_with_path() {
        let r = FakeReader::default();
        let v = list(vec![app(&r, "wp", vec![part(&r, "core", "")], vec![])]);
        let e = decode_apps(&v, &r).unwrap_err();
        assert_eq!(e.field, "[0].parts[0].label");
        assert!(e.problem.contains("empty"), "{}", e.problem);
    }

    #[test]
    fn rejects_wrong_nested_class_and_bad_drivers() {
        let r = FakeReader::default();
        let v = list(vec![app(
            &r,
            "wp",
            vec![part(&r, "core", "Core"), r.object("stdClass", vec![])],
            vec![],
        )]);
        assert_eq!(decode_apps(&v, &r).unwrap_err().field, "[0].parts[1]");

        let v = list(vec![app(&r, "wp", vec![], vec!["zip", "Bad"])]);
        assert_eq!(decode_apps(&v, &r).unwrap_err().field, "[0].drivers[1]");
        let v = list(vec![app(&r, "wp", vec![], vec!["zip", "zip"])]);
        assert_eq!(decode_apps(&v, &r).unwrap_err().field, "[0].drivers[1]");
    }

    #[test]
    fn rejects_missing_and_mistyped_property() {
        let r = FakeReader::default();
        let v = list(vec![r.object(APP, vec![("id", s("wp"))])]);
        let e = decode_apps(&v, &r).unwrap_err();
        assert_eq!(e.field, "[0].label");
        assert!(e.problem.contains("missing"), "{}", e.problem);
        let v = list(vec![r.object(APP, vec![("id", Value::Int(3))])]);
        assert_eq!(decode_apps(&v, &r).unwrap_err().field, "[0].id");
    }

    fn command(r: &FakeReader, name: &str, handler: Value, extra: Vec<(&str, Value)>) -> Value {
        let mut props = vec![
            ("name", s(name)),
            ("description", s("Does a thing")),
            ("handler", handler),
            ("arguments", list(vec![])),
            ("options", list(vec![])),
        ];
        for (k, v) in extra {
            props.retain(|(name, _)| *name != k);
            props.push((k, v));
        }
        r.object(COMMAND, props)
    }

    #[test]
    fn decodes_a_command_and_keeps_handler() {
        let r = FakeReader::default();
        let handler: Rc<dyn Any> = Rc::new(7u8);
        let arg = r.object(
            "SpeedySpec\\Cli\\Contracts\\CommandArgument",
            vec![
                ("name", s("target")),
                ("description", s("Where")),
                ("required", Value::Bool(false)),
            ],
        );
        let opt = r.object(
            "SpeedySpec\\Cli\\Contracts\\CommandOption",
            vec![
                ("name", s("format")),
                ("description", s("")),
                ("takesValue", Value::Bool(true)),
                ("default", s("json")),
            ],
        );
        let opt2 = r.object(
            "SpeedySpec\\Cli\\Contracts\\CommandOption",
            vec![
                ("name", s("force")),
                ("description", s("")),
                ("takesValue", Value::Bool(false)),
                ("default", Value::Null),
            ],
        );
        let c = command(
            &r,
            "build",
            Value::Opaque(handler.clone()),
            vec![
                ("arguments", list(vec![arg])),
                ("options", list(vec![opt, opt2])),
            ],
        );
        let cmds = decode_commands(&list(vec![c]), &r).unwrap();
        assert_eq!(cmds[0].name, "build");
        assert_eq!(cmds[0].description, "Does a thing");
        let Value::Opaque(kept) = &cmds[0].handler else {
            panic!("handler lost")
        };
        assert!(Rc::ptr_eq(kept, &handler));
        assert_eq!(cmds[0].arguments[0].name, "target");
        assert!(!cmds[0].arguments[0].required);
        assert_eq!(cmds[0].options[0].default.as_deref(), Some("json"));
        assert!(cmds[0].options[0].takes_value);
        assert_eq!(cmds[0].options[1].default, None);
    }

    #[test]
    fn rejects_command_without_callable_handler() {
        let r = FakeReader::default();
        let v = list(vec![command(&r, "build", Value::Null, vec![])]);
        let e = decode_commands(&v, &r).unwrap_err();
        assert_eq!(e.contract, "Command");
        assert_eq!(e.field, "[0].handler");
    }

    #[test]
    fn rejects_duplicate_command_arguments_and_options() {
        let r = FakeReader::default();
        let mk = |n: &str| {
            r.object(
                "SpeedySpec\\Cli\\Contracts\\CommandArgument",
                vec![
                    ("name", s(n)),
                    ("description", s("")),
                    ("required", Value::Bool(true)),
                ],
            )
        };
        let h = Value::Opaque(Rc::new(1u8));
        let c = command(
            &r,
            "x",
            h,
            vec![("arguments", list(vec![mk("a"), mk("a")]))],
        );
        let e = decode_commands(&list(vec![c]), &r).unwrap_err();
        assert_eq!(e.field, "[0].arguments[1].name");
    }

    #[test]
    fn single_item_decoders_report_relative_fields() {
        let r = FakeReader::default();
        let app_value = app(&r, "wp", vec![part(&r, "core", "")], vec![]);
        let e = decode_app(&app_value, &r).unwrap_err();
        assert_eq!(e.field, "parts[0].label");
        assert_eq!(
            decode_app(&app(&r, "wp", vec![], vec!["zip"]), &r)
                .unwrap()
                .drivers,
            ["zip"]
        );
        let e = decode_app(&Value::Null, &r).unwrap_err();
        assert_eq!(e.field, "");
        assert!(e.problem.contains(APP), "{}", e.problem);

        let h = Value::Opaque(Rc::new(1u8));
        assert_eq!(
            decode_command(&command(&r, "x", h, vec![]), &r)
                .unwrap()
                .name,
            "x"
        );
        let e = decode_command(&command(&r, "x", Value::Null, vec![]), &r).unwrap_err();
        assert_eq!(e.field, "handler");
    }

    #[test]
    fn error_displays_field_and_problem() {
        let e = ContractError {
            contract: "App",
            field: "[0].id".into(),
            problem: "bad".into(),
        };
        assert_eq!(e.to_string(), "[0].id: bad");
        let e = ContractError {
            field: String::new(),
            ..e
        };
        assert_eq!(e.to_string(), "bad");
    }
}
