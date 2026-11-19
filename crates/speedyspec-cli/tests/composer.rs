//! `speedyspec composer`: a plugin's composer packages, installed by the CLI itself from its composer.lock, and
//! autoloaded in Rust when the plugin loads.

mod common;

use std::fs;
use std::path::Path;

use common::{Run, Speedyspec};
use serde_json::{Value as Json, json};
use speedyspec_testkit::{Entry, Route, Server, build_zip};
use tempfile::TempDir;

/// A dist holding `Acme\Http\Client` (PSR-4), a files-autoloaded function and a classmapped class.
fn dist() -> Vec<u8> {
    build_zip(&[
        ("acme-http-1/", Entry::Dir),
        ("acme-http-1/src/", Entry::Dir),
        (
            "acme-http-1/src/Client.php",
            Entry::file(
                "<?php\nnamespace Acme\\Http;\nfinal class Client { public function hello(): string { return 'hello from acme'; } }\n",
            ),
        ),
        (
            "acme-http-1/src/functions.php",
            Entry::file("<?php\nfunction acme_http_loaded(): bool { return true; }\n"),
        ),
        ("acme-http-1/legacy/", Entry::Dir),
        (
            "acme-http-1/legacy/mapped.php",
            Entry::file("<?php\nclass Acme_Mapped { const WHO = 'mapped'; }\n"),
        ),
    ])
}

fn locked(server: &Server, version: &str) -> Json {
    json!({
        "name": "acme/http", "version": version, "type": "library",
        "dist": {"type": "zip", "url": format!("{}/acme-http.zip", server.url()), "shasum": ""},
        "require": {"php": ">=8.1"},
        "autoload": {
            "psr-4": {"Acme\\Http\\": "src/"},
            "files": ["src/functions.php"],
            "classmap": ["legacy/"],
        },
    })
}

/// A CLI plugin whose command uses its composer package.
fn plugin(dir: &Path, server: &Server, name: &str, version: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("speedyspec-plugin.json"),
        json!({"name": format!("speedyspec-fixture/{name}"), "version": "1.0.0", "contracts": "^1.0", "entry": "plugin.php"})
            .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("plugin.php"),
        format!(
            r#"<?php
declare(strict_types=1);
use SpeedySpec\Cli\Contracts\Command;
use SpeedySpec\Hooks\Hook;
Hook::add('speedyspec_commands', static function (array $commands): array
{{
    $commands[] = new Command('{name}', 'Uses its composer package.', static function (array $a, array $o): int
    {{
        echo (new \Acme\Http\Client())->hello(), ' ', acme_http_loaded() ? 'files' : 'no', ' ', \Acme_Mapped::WHO, "\n";

        return 0;
    }});

    return $commands;
}});
"#
        ),
    )
    .unwrap();
    fs::write(
        dir.join("composer.json"),
        json!({"require": {"acme/http": "^1"}}).to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("composer.lock"),
        json!({"packages": [locked(server, version)]}).to_string(),
    )
    .unwrap();
}

fn server() -> Server {
    Server::start(vec![("/acme-http.zip", Route::ok(dist()))])
}

fn composer(home: &Path, args: &[&str]) -> Run {
    let mut all = vec!["--no-interaction", "composer"];
    all.extend_from_slice(args);
    Speedyspec::with_home(&[], home).run(&all)
}

fn run_plugin(home: &Path, dir: &Path, command: &str) -> Run {
    Speedyspec::with_home(&[], home).run(&[&format!("--plugin-dir={}", dir.display()), command])
}

#[test]
fn composer_install_for_a_development_directory() {
    let (home, plugins, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    let dir = plugins.path().join("uses-http");
    plugin(&dir, &server, "uses-http", "1.0.0");
    let out = composer(
        home.path(),
        &["install", "--working-dir", dir.to_str().unwrap()],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(dir.join("vendor/acme/http/src/Client.php").is_file());
    assert!(dir.join("vendor/speedyspec-autoload.json").is_file());
    assert!(
        !dir.join("vendor/autoload.php").exists(),
        "no PHP autoloader is generated"
    );
    let out = run_plugin(home.path(), &dir, "uses-http");
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "hello from acme files mapped\n");
}

#[test]
fn a_plugin_whose_packages_are_not_installed_loads_with_a_warning() {
    let (home, plugins, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    let dir = plugins.path().join("uses-http");
    plugin(&dir, &server, "uses-http", "1.0.0");
    let out = Speedyspec::with_home(&[], home.path()).run(&[
        &format!("--plugin-dir={}", dir.display()),
        "self",
        "plugin",
        "list",
    ]);
    assert!(!out.stderr.contains("skipped"), "{}", out.stderr);
    assert!(
        out.stderr
            .contains("speedyspec composer install --working-dir"),
        "{}",
        out.stderr
    );
}

/// A plugin that ships composer's own vendor/ (and requires its autoload.php itself) loads as it did before.
#[test]
fn a_plugin_with_its_own_autoloader_still_loads() {
    let (home, plugins) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let dir = plugins.path().join("own-autoloader");
    fs::create_dir_all(dir.join("vendor")).unwrap();
    fs::write(
        dir.join("speedyspec-plugin.json"),
        json!({"name": "speedyspec-fixture/own-autoloader", "version": "1.0.0", "contracts": "^1.0", "entry": "plugin.php"}).to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("vendor/autoload.php"),
        "<?php function own_autoloader_ran(): string { return 'own'; }\n",
    )
    .unwrap();
    fs::write(
        dir.join("plugin.php"),
        r#"<?php
declare(strict_types=1);
require __DIR__ . '/vendor/autoload.php';
use SpeedySpec\Cli\Contracts\Command;
use SpeedySpec\Hooks\Hook;
Hook::add('speedyspec_commands', static function (array $commands): array
{
    $commands[] = new Command('own-autoloader', 'Uses its own autoloader.', static function (array $a, array $o): int
    {
        echo own_autoloader_ran(), "\n";

        return 0;
    });

    return $commands;
});
"#,
    )
    .unwrap();
    // It requires packages and has no composer.lock: speedyspec cannot install them, but the plugin brings its own.
    fs::write(
        dir.join("composer.json"),
        json!({"require": {"acme/http": "^1"}}).to_string(),
    )
    .unwrap();
    let out = run_plugin(home.path(), &dir, "own-autoloader");
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "own\n");
}

#[test]
fn no_lock_with_requirements_is_refused() {
    let home = TempDir::new().unwrap();
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("composer.json"),
        json!({"require": {"acme/http": "^1"}}).to_string(),
    )
    .unwrap();
    let out = composer(
        home.path(),
        &["install", "--working-dir", dir.path().to_str().unwrap()],
    );
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("no composer.lock"), "{}", out.stderr);
}

#[test]
fn a_missing_extension_names_the_package() {
    let (home, server) = (TempDir::new().unwrap(), server());
    let dir = TempDir::new().unwrap();
    let mut package = locked(&server, "1.0.0");
    package["require"] = json!({"ext-definitely-not-loaded": "*"});
    fs::write(
        dir.path().join("composer.json"),
        json!({"require": {"acme/http": "^1"}}).to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("composer.lock"),
        json!({"packages": [package]}).to_string(),
    )
    .unwrap();
    let out = composer(
        home.path(),
        &["install", "--working-dir", dir.path().to_str().unwrap()],
    );
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("acme/http") && out.stderr.contains("definitely-not-loaded"),
        "{}",
        out.stderr
    );
}

#[test]
fn composer_dump_autoload_rebuilds_the_maps() {
    let (home, plugins, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    let dir = plugins.path().join("uses-http");
    plugin(&dir, &server, "uses-http", "1.0.0");
    assert_eq!(
        composer(
            home.path(),
            &["install", "--working-dir", dir.to_str().unwrap()]
        )
        .code,
        0
    );
    fs::remove_file(dir.join("vendor/speedyspec-autoload.json")).unwrap();
    let out = composer(
        home.path(),
        &["dump-autoload", "--working-dir", dir.to_str().unwrap()],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        run_plugin(home.path(), &dir, "uses-http").stdout,
        "hello from acme files mapped\n"
    );
}

#[test]
fn composer_show_lists_packages() {
    let (home, plugins, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    let dir = plugins.path().join("uses-http");
    plugin(&dir, &server, "uses-http", "1.0.0");
    assert_eq!(
        composer(
            home.path(),
            &["install", "--working-dir", dir.to_str().unwrap()]
        )
        .code,
        0
    );
    let out = composer(
        home.path(),
        &["show", "--working-dir", dir.to_str().unwrap()],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "acme/http\t1.0.0\tlibrary\n");
}

#[test]
fn composer_validate_names_problems() {
    let home = TempDir::new().unwrap();
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("composer.json"), "{}").unwrap();
    fs::write(
        dir.path().join("composer.lock"),
        json!({"packages": [{"name": "Bad Name", "version": "1", "dist": {"type": "zip", "url": "https://e/a.zip"}}]}).to_string(),
    )
    .unwrap();
    let out = composer(
        home.path(),
        &["validate", "--working-dir", dir.path().to_str().unwrap()],
    );
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("not a package name"), "{}", out.stderr);
    fs::write(dir.path().join("composer.lock"), "{\"packages\": []}").unwrap();
    let out = composer(
        home.path(),
        &["validate", "--working-dir", dir.path().to_str().unwrap()],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
}

#[test]
fn an_unsupported_composer_subcommand_is_a_usage_error() {
    let home = TempDir::new().unwrap();
    let out = composer(home.path(), &["update"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(
        out.stderr.contains("speedyspec composer supports install, dump-autoload, show and validate; run composer itself for update"),
        "{}",
        out.stderr
    );
}

#[test]
fn self_plugin_install_installs_dependencies() {
    let (home, plugins, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    let dir = plugins.path().join("uses-http");
    plugin(&dir, &server, "uses-http", "1.0.0");
    let mut entries = Vec::new();
    for entry in [
        "speedyspec-plugin.json",
        "plugin.php",
        "composer.json",
        "composer.lock",
    ] {
        entries.push((
            format!("uses-http/{entry}"),
            fs::read_to_string(dir.join(entry)).unwrap(),
        ));
    }
    let zip_entries: Vec<(&str, Entry)> = std::iter::once(("uses-http/", Entry::Dir))
        .chain(entries.iter().map(|(n, b)| (n.as_str(), Entry::file(b))))
        .collect();
    server.route("/plugin.zip", Route::ok(build_zip(&zip_entries)));
    let out = Speedyspec::with_home(&[], home.path()).run(&[
        "self",
        "plugin",
        "install",
        &format!("{}/plugin.zip", server.url()),
        "--yes",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let installed = home.path().join("plugins/speedyspec-fixture.uses-http");
    assert!(installed.join("vendor/acme/http/src/Client.php").is_file());
    let out = Speedyspec::with_home(&[], home.path()).run(&["uses-http"]);
    assert_eq!(
        out.stdout, "hello from acme files mapped\n",
        "{}",
        out.stderr
    );
}

#[test]
fn a_failed_dependency_install_removes_the_plugin() {
    let (home, plugins) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let server = Server::start(vec![("/acme-http.zip", Route::status(404))]);
    let dir = plugins.path().join("uses-http");
    plugin(&dir, &server, "uses-http", "1.0.0");
    let mut entries = Vec::new();
    for entry in [
        "speedyspec-plugin.json",
        "plugin.php",
        "composer.json",
        "composer.lock",
    ] {
        entries.push((
            format!("uses-http/{entry}"),
            fs::read_to_string(dir.join(entry)).unwrap(),
        ));
    }
    let zip_entries: Vec<(&str, Entry)> = std::iter::once(("uses-http/", Entry::Dir))
        .chain(entries.iter().map(|(n, b)| (n.as_str(), Entry::file(b))))
        .collect();
    server.route("/plugin.zip", Route::ok(build_zip(&zip_entries)));
    let out = Speedyspec::with_home(&[], home.path()).run(&[
        "self",
        "plugin",
        "install",
        &format!("{}/plugin.zip", server.url()),
        "--yes",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("acme/http") && out.stderr.contains("404"),
        "{}",
        out.stderr
    );
    assert!(
        !home
            .path()
            .join("plugins/speedyspec-fixture.uses-http")
            .exists()
    );
}

#[test]
fn two_versions_of_one_package_warn() {
    let (home, plugins, server) = (TempDir::new().unwrap(), TempDir::new().unwrap(), server());
    for (name, version) in [("first-http", "1.0.0"), ("second-http", "1.1.0")] {
        let dir = plugins.path().join(name);
        plugin(&dir, &server, name, version);
        assert_eq!(
            composer(
                home.path(),
                &["install", "--working-dir", dir.to_str().unwrap()]
            )
            .code,
            0
        );
    }
    let out = Speedyspec::with_home(&[], home.path()).run(&[
        &format!(
            "--plugin-dir={}",
            plugins.path().join("first-http").display()
        ),
        &format!(
            "--plugin-dir={}",
            plugins.path().join("second-http").display()
        ),
        "first-http",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr.contains("acme/http")
            && out.stderr.contains("1.0.0")
            && out.stderr.contains("1.1.0"),
        "{}",
        out.stderr
    );
}

// ----- composer plugins

const PLUGIN_SRC: &str = r#"<?php
namespace Acme\ComposerPlugin;

use Composer\Composer;
use Composer\EventDispatcher\EventSubscriberInterface;
use Composer\Installer\PackageEvent;
use Composer\Installer\PackageEvents;
use Composer\IO\IOInterface;
use Composer\Plugin\PluginInterface;
use Composer\Script\Event;
use Composer\Script\ScriptEvents;

final class Plugin implements PluginInterface, EventSubscriberInterface
{
    private static string $log = '';

    public static function log(string $line): void
    {
        file_put_contents(self::$log, $line . "\n", FILE_APPEND);
    }

    public function activate(Composer $composer, IOInterface $io): void
    {
        self::$log = dirname($composer->getConfig()->get('vendor-dir')) . '/plugin.log';
        self::log('activate ' . PluginInterface::PLUGIN_API_VERSION . ' ' . $composer->getPackage()->getName());
        $io->write('acme plugin active');
        if (getenv('ACME_UNSUPPORTED') === '1') {
            $composer->getInstallationManager()->addInstaller(new \stdClass());
        }
        if (getenv('ACME_FATAL') === '1') {
            acme_function_that_does_not_exist();
        }
    }

    public function deactivate(Composer $composer, IOInterface $io): void
    {
    }

    public function uninstall(Composer $composer, IOInterface $io): void
    {
    }

    public static function getSubscribedEvents(): array
    {
        return [
            PackageEvents::POST_PACKAGE_INSTALL => 'onPackage',
            ScriptEvents::POST_AUTOLOAD_DUMP => ['onDump', 5],
        ];
    }

    public function onPackage(PackageEvent $event): void
    {
        $package = $event->getOperation()->getPackage();
        $path = $event->getComposer()->getInstallationManager()->getInstallPath($package);
        self::log('package ' . $package->getName() . ' ' . $package->getVersion() . ' ' . (is_dir($path) ? 'present' : 'missing'));
    }

    public function onDump(Event $event): void
    {
        self::log('dump ' . $event->getName() . ' ' . ($event->getIO() instanceof IOInterface ? 'io' : 'no io'));
    }
}
"#;

const SCRIPTS_SRC: &str = r#"<?php
namespace Acme\ComposerPlugin;

use Composer\Script\Event;

final class Scripts
{
    public static function installed(Event $event): void
    {
        Plugin::log('script ' . $event->getName() . ' ' . count($event->getComposer()->getRepositoryManager()->getLocalRepository()->getPackages()));
    }
}
"#;

fn plugin_dist() -> Vec<u8> {
    build_zip(&[
        ("acme-composer-plugin-1/", Entry::Dir),
        ("acme-composer-plugin-1/src/", Entry::Dir),
        (
            "acme-composer-plugin-1/src/Plugin.php",
            Entry::file(PLUGIN_SRC),
        ),
        (
            "acme-composer-plugin-1/src/Scripts.php",
            Entry::file(SCRIPTS_SRC),
        ),
    ])
}

/// A directory whose lock holds a composer plugin and a library, with root scripts.
fn with_composer_plugin(server: &Server) -> TempDir {
    server.route("/acme-composer-plugin.zip", Route::ok(plugin_dist()));
    let dir = TempDir::new().unwrap();
    let plugin = json!({
        "name": "acme/composer-plugin", "version": "1.0.0", "type": "composer-plugin",
        "dist": {"type": "zip", "url": format!("{}/acme-composer-plugin.zip", server.url()), "shasum": ""},
        "require": {"composer-plugin-api": "^2.0"},
        "autoload": {"psr-4": {"Acme\\ComposerPlugin\\": "src/"}},
        "extra": {"class": "Acme\\ComposerPlugin\\Plugin"},
    });
    fs::write(
        dir.path().join("composer.json"),
        json!({
            "name": "acme/site",
            "require": {"acme/composer-plugin": "^1", "acme/http": "^1"},
            "scripts": {"post-install-cmd": ["Acme\\ComposerPlugin\\Scripts::installed", "echo hi"]},
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("composer.lock"),
        json!({"packages": [plugin, locked(server, "1.0.0")]}).to_string(),
    )
    .unwrap();
    dir
}

fn log(dir: &Path) -> String {
    fs::read_to_string(dir.join("plugin.log")).unwrap_or_default()
}

#[test]
fn a_composer_plugin_is_activated_and_hears_the_events() {
    let (home, server) = (TempDir::new().unwrap(), server());
    let dir = with_composer_plugin(&server);
    let out = composer(
        home.path(),
        &[
            "install",
            "--allow-plugins",
            "--working-dir",
            dir.path().to_str().unwrap(),
        ],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        log(dir.path()),
        "activate 2.6.0 acme/site\npackage acme/http 1.0.0 present\ndump post-autoload-dump io\nscript post-install-cmd 2\n"
    );
    assert!(out.stderr.contains("acme plugin active"), "{}", out.stderr);
}

#[test]
fn composer_plugins_need_permission() {
    let (home, server) = (TempDir::new().unwrap(), server());
    let dir = with_composer_plugin(&server);
    let out = composer(
        home.path(),
        &["install", "--working-dir", dir.path().to_str().unwrap()],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(log(dir.path()), "");
    assert!(
        out.stderr.contains("composer plugins were not run: acme/composer-plugin (run with --allow-plugins to run them)"),
        "{}",
        out.stderr
    );
}

#[test]
fn a_shell_script_is_reported_not_run() {
    let (home, server) = (TempDir::new().unwrap(), server());
    let dir = with_composer_plugin(&server);
    let out = composer(
        home.path(),
        &[
            "install",
            "--allow-plugins",
            "--working-dir",
            dir.path().to_str().unwrap(),
        ],
    );
    assert!(
        out.stderr
            .contains("post-install-cmd: not run, speedyspec runs only PHP callables: echo hi"),
        "{}",
        out.stderr
    );
}

#[test]
fn an_unsupported_composer_api_names_it() {
    let (home, server) = (TempDir::new().unwrap(), server());
    let dir = with_composer_plugin(&server);
    let out = Speedyspec::with_home(&[], home.path())
        .env("ACME_UNSUPPORTED", "1")
        .run(&[
            "--no-interaction",
            "composer",
            "install",
            "--allow-plugins",
            "--working-dir",
            dir.path().to_str().unwrap(),
        ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("acme/composer-plugin")
            && out.stderr.contains(
                "composer's InstallationManager::addInstaller is not supported by speedyspec"
            ),
        "{}",
        out.stderr
    );
}

#[test]
fn a_composer_plugin_fatal_error_fails_the_install() {
    let (home, server) = (TempDir::new().unwrap(), server());
    let dir = with_composer_plugin(&server);
    let out = Speedyspec::with_home(&[], home.path())
        .env("ACME_FATAL", "1")
        .run(&[
            "--no-interaction",
            "composer",
            "install",
            "--allow-plugins",
            "--working-dir",
            dir.path().to_str().unwrap(),
        ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("acme/composer-plugin")
            && out.stderr.contains("acme_function_that_does_not_exist"),
        "{}",
        out.stderr
    );
}
