//! `speedyspec app ...`.

use std::io::Write;

use speedyspec_driver::App;

/// Drivers this binary can run. None yet: the zip driver arrives in a later plan.
const IMPLEMENTED_DRIVERS: &[&str] = &[];

/// `app list`: one line per app, `<id>\t<label>\t<drivers>`. A driver this binary does not
/// implement is marked with a trailing `?`, so a listed app never promises more than it can do.
pub fn list(apps: &[App]) -> i32 {
    if apps.is_empty() {
        eprintln!("No apps. Install a plugin that provides one.");
        return 0;
    }
    let mut out = std::io::stdout().lock();
    for app in apps {
        let drivers: Vec<String> = app
            .drivers
            .iter()
            .map(|driver| {
                if IMPLEMENTED_DRIVERS.contains(&driver.as_str()) {
                    driver.clone()
                } else {
                    format!("{driver}?")
                }
            })
            .collect();
        let _ = writeln!(out, "{}\t{}\t{}", app.id, app.label, drivers.join(","));
    }
    0
}
