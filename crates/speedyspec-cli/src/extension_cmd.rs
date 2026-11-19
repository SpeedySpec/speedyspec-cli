//! `speedyspec plugin` and `speedyspec theme`: list, install, update and delete an install's plugin and theme files
//! (see `extensions.rs`). Activating is the app's own business and is done in the app.

use std::io::Write;

use clap::{Arg, ArgAction, ArgMatches, Command as Clap};
use speedyspec_driver::{Kind, slug_problem};

use crate::boot::Runtime;
use crate::extensions::{Shelf, Updated};
use crate::prompt::confirm;

fn install_arg() -> Arg {
    Arg::new("install")
        .long("install")
        .required(true)
        .value_name("NAME")
        .help("The install, as app installs lists it")
}

fn slug_arg(help: &'static str) -> Arg {
    Arg::new("slug").value_name("SLUG").help(help)
}

pub fn clap(kind: Kind) -> Clap {
    let (name, about, things) = match kind {
        Kind::Plugin => ("plugin", "Work with an install's plugins", "plugin"),
        Kind::Theme => ("theme", "Work with an install's themes", "theme"),
    };
    Clap::new(name)
        .about(about)
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Clap::new("list")
                .about(format!("List the installed {things}s: slug, name, version"))
                .arg(install_arg()),
        )
        .subcommand(
            Clap::new("install")
                .about(format!("Download a {things} and put it in place"))
                .arg(
                    slug_arg("The slug, optionally with @VERSION (the latest by default)")
                        .required(true),
                )
                .arg(install_arg()),
        )
        .subcommand(
            Clap::new("update")
                .about(format!("Replace a {things} with its latest version"))
                .arg(slug_arg("The slug").required_unless_present("all"))
                .arg(
                    Arg::new("all")
                        .long("all")
                        .action(ArgAction::SetTrue)
                        .conflicts_with("slug")
                        .help(format!("Update every installed {things} a source has")),
                )
                .arg(install_arg()),
        )
        .subcommand(
            Clap::new("delete")
                .about(format!("Remove a {things}'s files"))
                .arg(slug_arg("The slug").required(true))
                .arg(
                    Arg::new("yes")
                        .long("yes")
                        .action(ArgAction::SetTrue)
                        .help("Do not ask for confirmation"),
                )
                .arg(install_arg()),
        )
}

/// `slug` or `slug@version`, checked before anything is looked up.
fn parse_spec(text: &str) -> Result<(String, Option<String>), String> {
    let (slug, version) = match text.split_once('@') {
        Some((slug, version)) => (slug, Some(version)),
        None => (text, None),
    };
    if let Some(problem) = slug_problem(slug) {
        return Err(format!("{text:?}: a slug {problem}"));
    }
    if let Some(version) = version
        && (version.is_empty()
            || version.len() > 64
            || version
                .chars()
                .any(|c| c.is_whitespace() || c == '/' || c == '\\'))
    {
        return Err(format!(
            "{text:?}: the version after @ must be 1 to 64 characters with no whitespace or slash"
        ));
    }
    Ok((slug.to_owned(), version.map(str::to_owned)))
}

fn usage(message: &str) -> i32 {
    eprintln!("speedyspec: {message}");
    2
}

fn failure(message: &str) -> i32 {
    eprintln!("speedyspec: {message}");
    1
}

pub fn run(runtime: &Runtime, kind: Kind, matches: &ArgMatches) -> i32 {
    let (sub, args) = matches.subcommand().expect("clap requires a subcommand");
    let name = args.get_one::<String>("install").expect("required");
    // Slugs are checked before the install is looked up or any plugin is asked.
    let spec = match args.try_get_one::<String>("slug").ok().flatten() {
        Some(text) => match parse_spec(text) {
            Ok(spec) => Some(spec),
            Err(message) => return usage(&message),
        },
        None => None,
    };
    if sub != "install" && spec.as_ref().is_some_and(|(_, v)| v.is_some()) {
        return usage("a version can be given only to install");
    }
    let shelf = match Shelf::open(runtime, name, kind) {
        Ok(shelf) => shelf,
        Err(message) => return failure(&message),
    };
    let mut out = std::io::stdout().lock();
    let kind_name = kind.as_str();
    match sub {
        "list" => match shelf.list() {
            Ok(found) => {
                for e in found {
                    let _ = writeln!(
                        out,
                        "{}\t{}\t{}",
                        e.slug,
                        e.name,
                        e.version.unwrap_or_default()
                    );
                }
                0
            }
            Err(message) => failure(&message),
        },
        "install" => {
            let (slug, version) = spec.expect("required");
            match shelf.install(&slug, version.as_deref()) {
                Ok(version) => {
                    let _ = writeln!(out, "installed {kind_name} {slug} {version}");
                    0
                }
                Err(message) => failure(&message),
            }
        }
        "update" => match spec {
            Some((slug, _)) => match shelf.update(&slug) {
                Ok(updated) => {
                    report(&mut out, kind_name, &slug, &updated);
                    0
                }
                Err(message) => failure(&message),
            },
            None => update_all(&shelf, &mut out),
        },
        "delete" => {
            let (slug, _) = spec.expect("required");
            let question = format!(
                "Delete the {kind_name} {slug} from {}? [y/N]",
                shelf.install_name()
            );
            if let Err(code) = confirm(args, &question) {
                return code;
            }
            match shelf.delete(&slug) {
                Ok(()) => {
                    let _ = writeln!(out, "deleted {kind_name} {slug}");
                    0
                }
                Err(message) => failure(&message),
            }
        }
        _ => unreachable!("clap only matches the subcommands above"),
    }
}

fn report(out: &mut impl Write, kind: &str, slug: &str, updated: &Updated) {
    let _ = match updated {
        Updated::AlreadyAt(version) => writeln!(out, "{slug} is already at {version}"),
        Updated::From { old, new } => writeln!(
            out,
            "updated {kind} {slug} {} -> {new}",
            old.as_deref().unwrap_or("?")
        ),
    };
}

/// Updates every installed one a source has; one that fails is reported and the rest go on.
fn update_all(shelf: &Shelf<'_>, out: &mut impl Write) -> i32 {
    let installed = match shelf.list() {
        Ok(installed) => installed,
        Err(message) => return failure(&message),
    };
    let mut code = 0;
    for extension in installed {
        let slug = &extension.slug;
        match shelf.resolve(slug, None) {
            Ok(None) => {
                eprintln!("speedyspec: skipped {slug}: no source has it");
                continue;
            }
            Err(message) => {
                eprintln!("speedyspec: {slug}: {message}");
                code = 1;
                continue;
            }
            Ok(Some(_)) => {}
        }
        match shelf.update(slug) {
            Ok(updated) => report(out, shelf.kind.as_str(), slug, &updated),
            Err(message) => {
                eprintln!("speedyspec: {slug}: {message}");
                code = 1;
            }
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::parse_spec;

    #[test]
    fn specs() {
        assert_eq!(parse_spec("akismet").unwrap(), ("akismet".into(), None));
        assert_eq!(
            parse_spec("akismet@5.3.1").unwrap(),
            ("akismet".into(), Some("5.3.1".into()))
        );
        for bad in ["", "akismet@", "../x@1", "a@1 2", "a@1/2"] {
            assert!(parse_spec(bad).is_err(), "{bad:?}");
        }
    }
}
