//! Deciding what `app install` will do: which app, which parts, which driver, which option
//! values. Pure decisions; the only I/O is asking the user through a [`Prompter`].

use std::collections::BTreeMap;

use speedyspec_driver::{App, InstallOption};

use crate::prompt::Prompter;

/// What the user gave on the command line.
pub struct Given<'a> {
    pub app: Option<&'a str>,
    pub parts: &'a [String],
    pub driver: Option<&'a str>,
}

/// What will be installed, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choices {
    pub app: String,
    pub parts: Vec<String>,
    pub driver: String,
}

/// Why the choices could not be settled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveError {
    /// The command line is wrong or incomplete: exit 2.
    Usage(String),
    /// The command line is fine but the install cannot happen: exit 1.
    Failure(String),
}

/// Settles app, parts and driver. `usable_drivers` are the drivers this binary can run;
/// `prompter` is `None` when the CLI may not ask questions.
pub fn resolve_choices(
    given: &Given<'_>,
    apps: &[App],
    usable_drivers: &[&str],
    prompter: Option<&mut dyn Prompter>,
) -> Result<Choices, ResolveError> {
    let mut prompter = prompter;
    if apps.is_empty() {
        return Err(ResolveError::Failure(
            "No apps. Install a plugin that provides one.".to_owned(),
        ));
    }
    let known = || {
        apps.iter()
            .map(|app| app.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let app_id = match given.app {
        Some(id) => id.to_owned(),
        None => match prompter.as_deref_mut() {
            Some(prompter) => {
                let choices: Vec<(String, String)> = apps
                    .iter()
                    .map(|app| (app.id.clone(), app.label.clone()))
                    .collect();
                prompter
                    .choose("App to install", &choices)
                    .map_err(prompt_failed)?
            }
            None => return Err(ResolveError::Usage(format!("give an app: {}", known()))),
        },
    };
    let Some(app) = apps.iter().find(|app| app.id == app_id) else {
        return Err(ResolveError::Usage(format!(
            "there is no app \"{app_id}\"; the apps are: {}",
            known()
        )));
    };

    let part_ids = || {
        app.parts
            .iter()
            .map(|part| part.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let parts: Vec<String> = if given.parts.is_empty() {
        let defaults: Vec<String> = app
            .parts
            .iter()
            .filter(|part| part.default)
            .map(|part| part.id.clone())
            .collect();
        if defaults.is_empty() && !app.parts.is_empty() {
            return Err(ResolveError::Usage(format!(
                "{} has no default parts; give --part: {}",
                app.id,
                part_ids()
            )));
        }
        defaults
    } else {
        let mut chosen: Vec<String> = Vec::new();
        for part in given.parts {
            if !app.parts.iter().any(|known| known.id == *part) {
                return Err(ResolveError::Usage(format!(
                    "{} has no part \"{part}\"; its parts are: {}",
                    app.id,
                    part_ids()
                )));
            }
            if !chosen.contains(part) {
                chosen.push(part.clone());
            }
        }
        chosen
    };

    let usable: Vec<&str> = app
        .drivers
        .iter()
        .map(String::as_str)
        .filter(|driver| usable_drivers.contains(driver))
        .collect();
    if usable.is_empty() {
        return Err(ResolveError::Failure(format!(
            "{} can only be installed with {}, which this speedyspec cannot run",
            app.id,
            if app.drivers.is_empty() {
                "no driver".to_owned()
            } else {
                app.drivers.join(", ")
            }
        )));
    }
    let driver = match given.driver {
        Some(driver) if usable.contains(&driver) => driver.to_owned(),
        Some(driver) => {
            return Err(ResolveError::Usage(format!(
                "{} cannot be installed with --driver {driver}; use: {}",
                app.id,
                usable.join(", ")
            )));
        }
        None if usable.len() == 1 => usable[0].to_owned(),
        None => match prompter.as_deref_mut() {
            Some(prompter) => {
                let choices: Vec<(String, String)> = usable
                    .iter()
                    .map(|driver| ((*driver).to_owned(), String::new()))
                    .collect();
                prompter.choose("Driver", &choices).map_err(prompt_failed)?
            }
            None => {
                return Err(ResolveError::Usage(format!(
                    "give --driver: {}",
                    usable.join(", ")
                )));
            }
        },
    };

    Ok(Choices {
        app: app.id.clone(),
        parts,
        driver,
    })
}

fn prompt_failed(error: crate::prompt::PromptError) -> ResolveError {
    ResolveError::Failure(error.to_string())
}

/// Settles a value for each declared option: `(name, value)` in declaration order. An optional
/// option that ends up without a value is left out.
pub fn resolve_options(
    declared: &[InstallOption],
    given: &BTreeMap<String, String>,
    prompter: Option<&mut dyn Prompter>,
) -> Result<Vec<(String, String)>, ResolveError> {
    let mut prompter = prompter;
    if let Some(unknown) = given
        .keys()
        .find(|name| !declared.iter().any(|option| option.name == **name))
    {
        return Err(ResolveError::Usage(format!(
            "there is no install option \"{unknown}\""
        )));
    }
    let mut resolved = Vec::new();
    let mut missing = Vec::new();
    for option in declared {
        let default = option.default.as_deref();
        let value = match (given.get(&option.name), prompter.as_deref_mut()) {
            (Some(value), _) => Some(value.clone()),
            (None, Some(prompter)) => loop {
                let answer = prompter
                    .text(&option.label, default, option.secret)
                    .map_err(prompt_failed)?;
                if !answer.is_empty() {
                    break Some(answer);
                }
                if !option.required {
                    break None;
                }
            },
            (None, None) => default.map(str::to_owned),
        };
        match value {
            Some(value) => resolved.push((option.name.clone(), value)),
            None if option.required => missing.push(format!("--{}", option.name)),
            None => {}
        }
    }
    if !missing.is_empty() {
        return Err(ResolveError::Usage(format!(
            "missing required install options: {}",
            missing.join(", ")
        )));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::PromptError;
    use speedyspec_driver::AppPart;
    use std::collections::VecDeque;

    /// Answers from a queue and remembers what it was asked.
    #[derive(Default)]
    struct Scripted {
        answers: VecDeque<String>,
        /// `(label, choice ids or default, secret)`
        asked: Vec<(String, Vec<String>, bool)>,
    }

    impl Scripted {
        fn answering(answers: &[&str]) -> Scripted {
            Scripted {
                answers: answers.iter().map(|a| (*a).to_owned()).collect(),
                asked: Vec::new(),
            }
        }
    }

    impl Prompter for Scripted {
        fn choose(
            &mut self,
            label: &str,
            choices: &[(String, String)],
        ) -> Result<String, PromptError> {
            let ids = choices.iter().map(|(id, _)| id.clone()).collect();
            self.asked.push((label.to_owned(), ids, false));
            self.answers
                .pop_front()
                .ok_or_else(|| PromptError("no scripted answer".to_owned()))
        }

        fn text(
            &mut self,
            label: &str,
            default: Option<&str>,
            secret: bool,
        ) -> Result<String, PromptError> {
            let shown = default.map(str::to_owned).into_iter().collect();
            self.asked.push((label.to_owned(), shown, secret));
            let answer = self
                .answers
                .pop_front()
                .ok_or_else(|| PromptError("no scripted answer".to_owned()))?;
            // As the terminal prompter does: an empty answer means the default.
            Ok(if answer.is_empty() {
                default.unwrap_or_default().to_owned()
            } else {
                answer
            })
        }
    }

    fn part(id: &str, default: bool) -> AppPart {
        AppPart {
            id: id.to_owned(),
            label: id.to_uppercase(),
            default,
        }
    }

    fn app(id: &str, parts: Vec<AppPart>, drivers: &[&str]) -> App {
        App {
            id: id.to_owned(),
            label: id.to_uppercase(),
            parts,
            drivers: drivers.iter().map(|d| (*d).to_owned()).collect(),
        }
    }

    fn site() -> App {
        app(
            "site",
            vec![part("web", true), part("docs", false)],
            &["zip"],
        )
    }

    fn given<'a>(app: Option<&'a str>, parts: &'a [String], driver: Option<&'a str>) -> Given<'a> {
        Given { app, parts, driver }
    }

    fn usage(result: Result<impl std::fmt::Debug, ResolveError>) -> String {
        match result {
            Err(ResolveError::Usage(message)) => message,
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    fn failure(result: Result<impl std::fmt::Debug, ResolveError>) -> String {
        match result {
            Err(ResolveError::Failure(message)) => message,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    fn option(name: &str, default: Option<&str>, required: bool, secret: bool) -> InstallOption {
        InstallOption {
            name: name.to_owned(),
            label: name.to_uppercase(),
            default: default.map(str::to_owned),
            required,
            secret,
        }
    }

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn uses_given_app_parts_and_driver() {
        let parts = ["docs".to_owned()];
        let choices = resolve_choices(
            &given(Some("site"), &parts, Some("zip")),
            &[site()],
            &["zip"],
            None,
        )
        .unwrap();
        assert_eq!(
            choices,
            Choices {
                app: "site".to_owned(),
                parts: vec!["docs".to_owned()],
                driver: "zip".to_owned(),
            }
        );
    }

    #[test]
    fn unknown_app_is_usage_error_listing_apps() {
        let apps = [site(), app("blog", vec![], &["zip"])];
        let message = usage(resolve_choices(
            &given(Some("nope"), &[], None),
            &apps,
            &["zip"],
            None,
        ));
        assert!(
            message.contains("nope") && message.contains("site") && message.contains("blog"),
            "{message}"
        );
    }

    #[test]
    fn no_apps_is_a_failure() {
        let message = failure(resolve_choices(
            &given(None, &[], None),
            &[],
            &["zip"],
            None,
        ));
        assert_eq!(message, "No apps. Install a plugin that provides one.");
        let message = failure(resolve_choices(
            &given(Some("site"), &[], None),
            &[],
            &["zip"],
            None,
        ));
        assert_eq!(message, "No apps. Install a plugin that provides one.");
    }

    #[test]
    fn missing_app_prompts_when_interactive() {
        let apps = [site(), app("blog", vec![], &["zip"])];
        let mut prompter = Scripted::answering(&["blog"]);
        let choices = resolve_choices(
            &given(None, &[], None),
            &apps,
            &["zip"],
            Some(&mut prompter),
        )
        .unwrap();
        assert_eq!(choices.app, "blog");
        assert_eq!(prompter.asked.len(), 1);
        assert_eq!(prompter.asked[0].1, vec!["site", "blog"]);
    }

    #[test]
    fn missing_app_without_prompter_is_usage_error() {
        let message = usage(resolve_choices(
            &given(None, &[], None),
            &[site()],
            &["zip"],
            None,
        ));
        assert!(message.contains("site"), "{message}");
    }

    #[test]
    fn defaults_to_default_parts() {
        let choices =
            resolve_choices(&given(Some("site"), &[], None), &[site()], &["zip"], None).unwrap();
        assert_eq!(choices.parts, vec!["web"]);
    }

    #[test]
    fn unknown_part_is_usage_error() {
        let parts = ["nope".to_owned()];
        let message = usage(resolve_choices(
            &given(Some("site"), &parts, None),
            &[site()],
            &["zip"],
            None,
        ));
        assert!(
            message.contains("nope") && message.contains("web") && message.contains("docs"),
            "{message}"
        );
    }

    #[test]
    fn no_default_parts_requires_part_flag() {
        let apps = [app("a", vec![part("x", false), part("y", false)], &["zip"])];
        let mut prompter = Scripted::default();
        let message = usage(resolve_choices(
            &given(Some("a"), &[], None),
            &apps,
            &["zip"],
            Some(&mut prompter),
        ));
        assert!(
            message.contains("--part") && message.contains('x') && message.contains('y'),
            "{message}"
        );
        assert!(prompter.asked.is_empty(), "parts are never prompted for");
    }

    #[test]
    fn app_without_parts_installs_with_none() {
        let apps = [app("a", vec![], &["zip"])];
        let choices = resolve_choices(&given(Some("a"), &[], None), &apps, &["zip"], None).unwrap();
        assert!(choices.parts.is_empty());
    }

    #[test]
    fn single_usable_driver_is_chosen() {
        let apps = [app("a", vec![], &["zip", "kubernetes"])];
        let choices = resolve_choices(&given(Some("a"), &[], None), &apps, &["zip"], None).unwrap();
        assert_eq!(choices.driver, "zip");
    }

    #[test]
    fn several_usable_drivers_prompt_or_require_flag() {
        let apps = [app("a", vec![], &["zip", "kubernetes"])];
        let usable = ["zip", "kubernetes"];

        let message = usage(resolve_choices(
            &given(Some("a"), &[], None),
            &apps,
            &usable,
            None,
        ));
        assert!(
            message.contains("--driver")
                && message.contains("zip")
                && message.contains("kubernetes"),
            "{message}"
        );

        let mut prompter = Scripted::answering(&["kubernetes"]);
        let choices = resolve_choices(
            &given(Some("a"), &[], None),
            &apps,
            &usable,
            Some(&mut prompter),
        )
        .unwrap();
        assert_eq!(choices.driver, "kubernetes");
        assert_eq!(prompter.asked[0].1, vec!["zip", "kubernetes"]);
    }

    #[test]
    fn driver_not_usable_is_reported() {
        let apps = [app("cluster", vec![], &["kubernetes"])];
        let message = failure(resolve_choices(
            &given(Some("cluster"), &[], None),
            &apps,
            &["zip"],
            None,
        ));
        assert!(
            message.contains("kubernetes") && message.contains("cluster"),
            "{message}"
        );
    }

    #[test]
    fn given_driver_must_be_usable_for_the_app() {
        let apps = [app("a", vec![], &["zip", "kubernetes"])];
        // Listed by the app, but this binary cannot run it.
        let message = usage(resolve_choices(
            &given(Some("a"), &[], Some("kubernetes")),
            &apps,
            &["zip"],
            None,
        ));
        assert!(
            message.contains("kubernetes") && message.contains("zip"),
            "{message}"
        );
        // Runnable, but the app does not list it.
        let apps = [app("b", vec![], &["zip"])];
        let message = usage(resolve_choices(
            &given(Some("b"), &[], Some("kubernetes")),
            &apps,
            &["zip", "kubernetes"],
            None,
        ));
        assert!(message.contains("kubernetes"), "{message}");
    }

    #[test]
    fn options_given_beat_defaults() {
        let declared = [option("title", Some("Untitled"), false, false)];
        let resolved = resolve_options(&declared, &values(&[("title", "Hi")]), None).unwrap();
        assert_eq!(resolved, vec![("title".to_owned(), "Hi".to_owned())]);
    }

    #[test]
    fn options_fall_back_to_defaults_without_prompter() {
        let declared = [option("title", Some("Untitled"), true, false)];
        let resolved = resolve_options(&declared, &values(&[]), None).unwrap();
        assert_eq!(resolved, vec![("title".to_owned(), "Untitled".to_owned())]);
    }

    #[test]
    fn missing_required_options_are_listed_together() {
        let declared = [
            option("title", None, true, false),
            option("email", None, true, false),
            option("theme", None, false, false),
        ];
        let message = usage(resolve_options(&declared, &values(&[]), None));
        assert!(
            message.contains("--title") && message.contains("--email"),
            "{message}"
        );
        assert!(!message.contains("--theme"), "{message}");
    }

    #[test]
    fn undeclared_given_option_is_usage_error() {
        let declared = [option("title", None, false, false)];
        let message = usage(resolve_options(
            &declared,
            &values(&[("colour", "red")]),
            None,
        ));
        assert!(message.contains("colour"), "{message}");
    }

    #[test]
    fn prompts_for_options_and_hides_secrets() {
        let declared = [
            option("title", Some("Untitled"), false, false),
            option("token", None, false, true),
            option("email", None, true, false),
        ];
        let mut prompter = Scripted::answering(&["Hi", "s3cret", "a@b.test"]);
        // A given value is not asked for again.
        let resolved = resolve_options(&declared, &values(&[]), Some(&mut prompter)).unwrap();
        assert_eq!(
            resolved,
            vec![
                ("title".to_owned(), "Hi".to_owned()),
                ("token".to_owned(), "s3cret".to_owned()),
                ("email".to_owned(), "a@b.test".to_owned()),
            ]
        );
        let secrets: Vec<bool> = prompter.asked.iter().map(|asked| asked.2).collect();
        assert_eq!(secrets, vec![false, true, false]);
        assert_eq!(
            prompter.asked[0].1,
            vec!["Untitled"],
            "the default is offered"
        );
    }

    #[test]
    fn given_options_are_not_prompted_for() {
        let declared = [
            option("title", None, true, false),
            option("email", None, true, false),
        ];
        let mut prompter = Scripted::answering(&["a@b.test"]);
        let resolved =
            resolve_options(&declared, &values(&[("title", "Hi")]), Some(&mut prompter)).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(prompter.asked.len(), 1);
        assert_eq!(prompter.asked[0].0, "EMAIL");
    }

    #[test]
    fn empty_answer_uses_default_or_asks_again() {
        // Optional with a default: an empty answer takes the default.
        let declared = [option("title", Some("Untitled"), false, false)];
        let mut prompter = Scripted::answering(&[""]);
        let resolved = resolve_options(&declared, &values(&[]), Some(&mut prompter)).unwrap();
        assert_eq!(resolved, vec![("title".to_owned(), "Untitled".to_owned())]);

        // Required without a default: asked again until there is an answer.
        let declared = [option("email", None, true, false)];
        let mut prompter = Scripted::answering(&["", "", "a@b.test"]);
        let resolved = resolve_options(&declared, &values(&[]), Some(&mut prompter)).unwrap();
        assert_eq!(resolved, vec![("email".to_owned(), "a@b.test".to_owned())]);
        assert_eq!(prompter.asked.len(), 3);
    }

    #[test]
    fn optional_option_without_value_is_omitted() {
        let declared = [option("theme", None, false, false)];
        assert_eq!(
            resolve_options(&declared, &values(&[]), None).unwrap(),
            vec![]
        );
        let mut prompter = Scripted::answering(&[""]);
        assert_eq!(
            resolve_options(&declared, &values(&[]), Some(&mut prompter)).unwrap(),
            vec![]
        );
    }
}
