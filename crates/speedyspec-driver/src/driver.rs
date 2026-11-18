//! What every install driver gives the CLI.
//!
//! A driver is a delivery mechanism the CLI owns (unpacking archives, applying resources to a
//! cluster). What is delivered comes from plugins: the CLI filters `speedyspec_driver_<id>` and
//! hands the driver the answer. Drivers depend on this crate and never on each other.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;

use speedyspec_hooks::Value;

use crate::contracts::{ContractError, ObjectReader};

/// A command-line flag a driver adds to `app install`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverFlag {
    /// Without the leading dashes.
    pub name: &'static str,
    pub help: &'static str,
    /// `false` for a switch.
    pub takes_value: bool,
    pub default: Option<&'static str>,
}

/// What the user gave a driver.
pub struct DriverInput<'a> {
    /// The app being installed and its selected parts, for drivers that name or label what
    /// they create after them.
    pub app: &'a str,
    pub parts: &'a [String],
    /// The driver's flags that were given, by name; a switch that was given is `"1"`.
    pub flags: &'a BTreeMap<String, String>,
    /// Whether the driver may ask the user questions.
    pub interactive: bool,
}

/// A driver's checked settings for one install.
pub struct Prepared {
    /// Shared with plugins in the install request, so never a secret.
    pub settings: Vec<(String, String)>,
    /// The driver's own, handed back to [`Driver::install`].
    pub state: Box<dyn Any>,
}

/// A plugin that answered a driver's filter, for drivers that record where an install came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginRef {
    pub name: String,
    pub version: String,
}

/// An install whose files are on this machine, which the CLI records so later commands can find
/// it by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalInstall {
    pub name: String,
    /// Absolute.
    pub location: std::path::PathBuf,
}

/// What the user gave `app uninstall`.
pub struct UninstallInput<'a> {
    /// The install's name.
    pub name: &'a str,
    /// The driver's uninstall flags that were given, by name; a switch that was given is `"1"`.
    pub flags: &'a BTreeMap<String, String>,
    pub interactive: bool,
}

/// What an uninstall will remove, found without changing anything.
pub struct UninstallPlan {
    /// Shown to the user before they confirm.
    pub summary: Vec<String>,
    /// The driver's own, handed back to [`Driver::uninstall`].
    pub state: Box<dyn Any>,
}

/// Why a driver could not prepare or install; the message is shown to the user as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverError(pub String);

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DriverError {}

/// Where a driver reports what it is doing, one line at a time.
pub trait Progress {
    fn step(&mut self, message: &str);
}

/// An install mechanism compiled into the binary.
pub trait Driver {
    /// Also names the filter plugins answer: `speedyspec_driver_<id>`.
    fn id(&self) -> &'static str;

    fn flags(&self) -> Vec<DriverFlag>;

    /// Checks the user's flags before any plugin is asked for anything.
    fn prepare(&self, input: &DriverInput<'_>) -> Result<Prepared, DriverError>;

    /// What the driver's filter starts from, before any plugin has answered: an empty list
    /// for a driver whose answer is a list, null for one whose answer is a single object.
    fn seed(&self) -> Value;

    /// Checks what one plugin callback returned on the driver's filter.
    fn check_answer(&self, answer: &Value, reader: &dyn ObjectReader) -> Result<(), ContractError>;

    /// Runs the install with the final, already checked answer. Returns what the
    /// `speedyspec_install_after` hook receives.
    fn install(
        &self,
        prepared: &Prepared,
        answer: &Value,
        reader: &dyn ObjectReader,
        plugins: &[PluginRef],
        progress: &mut dyn Progress,
    ) -> Result<Value, DriverError>;

    /// What `install` returned, as lines of command output for the user.
    fn report(&self, result: &Value) -> Vec<String>;

    /// The install `prepared` describes, when its files end up on this machine: the CLI records it
    /// under that name. `None` (the default) for drivers whose installs live elsewhere and keep
    /// their own record.
    fn local_install(&self, _prepared: &Prepared) -> Option<LocalInstall> {
        None
    }

    /// Flags `app uninstall` accepts for this driver. Empty means the driver cannot uninstall.
    fn uninstall_flags(&self) -> Vec<DriverFlag> {
        Vec::new()
    }

    /// Finds the install and says what removing it will delete. Changes nothing.
    fn plan_uninstall(&self, input: &UninstallInput<'_>) -> Result<UninstallPlan, DriverError> {
        let _ = input;
        Err(DriverError(format!(
            "the {} driver cannot uninstall",
            self.id()
        )))
    }

    /// Removes what `plan_uninstall` found. Returns lines of command output.
    fn uninstall(
        &self,
        plan: &UninstallPlan,
        progress: &mut dyn Progress,
    ) -> Result<Vec<String>, DriverError> {
        let _ = (plan, progress);
        Err(DriverError(format!(
            "the {} driver cannot uninstall",
            self.id()
        )))
    }
}
