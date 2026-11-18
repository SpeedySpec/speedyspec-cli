# speedyspec

`speedyspec` is a command line with a statically linked PHP 8.5 interpreter inside it. It knows
nothing about any particular application: PHP plugins, loaded at start-up, add subcommands and
installable "apps" through hooks. No PHP, Composer or `php.ini` is needed on the machine.

```
speedyspec --version              # speedyspec, php and contracts versions, three lines
speedyspec php <file> [args...]   # run a PHP file with hooks available; exit code is the script's
speedyspec app list               # id <TAB> label <TAB> drivers ("?" marks a driver this binary cannot run)
speedyspec app install <app> ...  # install an app a plugin provides (see "Installing an app")
speedyspec app uninstall <name>   # remove a Kubernetes install (see "Installing into Kubernetes")
speedyspec plugin list            # name <TAB> version <TAB> directory
speedyspec plugin install <src>   # install a plugin from a zip URL or a GitHub release
speedyspec plugin remove <name>   # remove an installed plugin
speedyspec self update [--check]  # replace this binary with the latest release (see "Updating speedyspec")
speedyspec <plugin command> ...   # whatever loaded plugins added
speedyspec                        # no arguments: help on stderr, exit 2
```

Global flags, accepted anywhere before a script's own arguments: `--no-plugins`,
`--no-interaction`, and `--plugin-dir <path>` (or `--plugin-dir=<path>`, repeatable).

Exit codes: 0 success, 1 failure, 2 usage error. Command output goes to stdout and every
diagnostic to stderr. speedyspec's own lines start with `speedyspec: `; PHP's diagnostics keep
PHP's log form (`PHP Warning:  ...`, `PHP Fatal error:  ...`), and usage errors and help come from
the argument parser (`error: ...`, `Usage: ...`). Help asked for with `--help` goes to stdout;
help shown because no command was given goes to stderr, with exit code 2.

## Download

Each release has one binary per platform, and a `SHA256SUMS` file listing their checksums. The
latest release's files are always at these addresses:

| Platform | Binary |
| --- | --- |
| Linux x86_64 (static) | https://github.com/SpeedySpec/speedyspec-cli/releases/latest/download/speedyspec-linux-x86_64 |
| Linux aarch64 (static) | https://github.com/SpeedySpec/speedyspec-cli/releases/latest/download/speedyspec-linux-aarch64 |
| macOS Apple silicon (12 or later) | https://github.com/SpeedySpec/speedyspec-cli/releases/latest/download/speedyspec-macos-aarch64 |
| macOS Intel (12 or later) | https://github.com/SpeedySpec/speedyspec-cli/releases/latest/download/speedyspec-macos-x86_64 |

Download the binary under its own name, check it, then make it executable:

```
base=https://github.com/SpeedySpec/speedyspec-cli/releases/latest/download
curl -fsSLO "$base/speedyspec-linux-x86_64" -O "$base/SHA256SUMS"
grep ' speedyspec-linux-x86_64$' SHA256SUMS | sha256sum -c     # macOS: shasum -a 256 -c
chmod +x speedyspec-linux-x86_64 && mv speedyspec-linux-x86_64 /usr/local/bin/speedyspec
```

The macOS binaries are not notarized. `curl` downloads run as they are; a file downloaded with a
browser is quarantined by macOS, which then refuses to open it until you clear the flag
(`xattr -d com.apple.quarantine speedyspec-macos-aarch64`). After that, `speedyspec self update`
keeps it current (see "Updating speedyspec").

## Building

The PHP contracts the plugins program against are a separate package,
[`speedyspec-cli-contracts`](https://github.com/SpeedySpec/speedyspec-cli-contracts). The binary
embeds one release of it, pinned in `contracts-version` (its version, the archive's URL and its
SHA-256). `scripts/fetch-contracts.sh` downloads that archive, checks it and unpacks it into
`.contracts/<version>/`, where `build.rs` reads it; the build stops with a message saying so if
it has not been fetched. To work on the contracts themselves, set `SPEEDYSPEC_CONTRACTS_DIR` to a
checkout instead (the build warns when its version is not the pinned one). To move the pin,
change all three lines of `contracts-version` (`curl -fsSL <url> | shasum -a 256` gives the
hash) and run the script again.

```
scripts/fetch-contracts.sh          # once per pin: the contracts into .contracts/
scripts/build-php.sh                # once: builds the static PHP into .php-build/ (slow)
cargo build --release -p speedyspec-cli
./target/release/speedyspec --version
cargo test --workspace
scripts/linux.sh cargo test --workspace   # the same, in a Linux (musl) container
scripts/smoke.sh target/release/speedyspec  # the checks a release binary must pass
```

See `docs/php-embed.md` for how PHP is built, linked and embedded.

### Releasing

Releases are cut by release-please. It keeps a pull request titled `chore: release <version>`
open on `main`, with the changelog and `version.txt` (the version `--version` prints) updated
from the Conventional Commits merged since the last release. Check the title of the first one
before merging it: it should be `chore: release 0.1.0`.

Merging it leaves `version.txt` naming a version with no tag yet (`scripts/release-pending.sh`
says so). The Release workflow then builds, tests and smoke-runs the four binaries at that
commit first, about half an hour, and only when all four pass lets release-please tag the
version and publish the GitHub release, uploading `SHA256SUMS` and then the binaries straight
after. A failed build publishes nothing: fix it, and the next push to `main` tries again. To
rebuild and re-upload the binaries of a release that already exists, run the Release workflow by
hand (*Run workflow*) with its tag.

## Plugins

A plugin is a directory with a `speedyspec-plugin.json` and an entry PHP file:

```json
{
    "name": "acme/greeter",
    "version": "1.0.0",
    "contracts": "^1.0",
    "entry": "plugin.php"
}
```

`name` is `vendor/package` in lowercase letters, digits and hyphens; `contracts` is the range of
the contracts package (`SpeedySpec\Cli\Contracts`) the plugin was written for, checked against the
version bundled in the binary (see `speedyspec --version`); `entry` is relative to the directory.

Plugins are found in the user directory (`$SPEEDYSPEC_HOME/plugins`, otherwise the platform data
directory's `speedyspec/plugins`) and in each `--plugin-dir`. `SPEEDYSPEC_HOME` must be an
absolute path; a relative one (or a relative `HOME` behind the data directory) is ignored with a
warning. A `--plugin-dir` that does not exist is reported. The working directory is never
searched. What a plugin prints while loading (`echo`, `print`, PHP's own diagnostics) goes to
stderr; the one exception is a direct write to `STDOUT` or `php://stdout`, which reaches stdout
(see `docs/php-embed.md`). A plugin that throws, hits a fatal error, calls `exit()` or needs
another contracts version is skipped with a `speedyspec: plugin <name> skipped: <why>` line; the
others still load.

```php
<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\Command;
use SpeedySpec\Cli\Contracts\CommandArgument;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_commands', static function (array $commands): array
{
    $commands[] = new Command(
        'hello',
        'Greets someone.',
        static function (array $arguments, array $options): int
        {
            echo "hello {$arguments['name']}\n";

            return 0;
        },
        [new CommandArgument('name', 'Who to greet.')],
    );

    return $commands;
});
```

A handler receives the arguments and options keyed by name (a flag option is `true` or `false`,
a value option its string, its default or `null`) and returns the exit code (0 to 255; any other
integer is reported as 1, never wrapped round to 0). An uncaught exception
exits 1 with `speedyspec: <command>: <class>: <message>`. A command may not reuse `app`, `plugin`,
`self`, `php` or `help`, or the name of a command an earlier plugin added (the first one wins);
it is dropped with a message naming the plugin.

## Installing an app

```
speedyspec app install [<app>] [--part <id>]... [--driver <id>] [--base <dir>] [--force] [--<option> <value>]...
```

The app, its parts and the driver are settled first: `--part` defaults to the app's default
parts; `--driver` can be left out when the app has exactly one driver this binary can run. The
drivers are `zip` and `kubernetes`. Anything missing is asked for when stdin is a terminal and
`--no-interaction` was not given; otherwise the command fails with what is missing (it never
waits for input from a script).

Plugins may declare install options for an app and driver. They become flags
(`--title "My site"`) when the arguments already name the app and the driver is known, so give
the app before its options: `app install site --title Hi`. `app install <app> --help` lists
them. Otherwise they are prompted for. A secret option is read without echo.

The zip driver downloads each archive a plugin names over HTTPS, checks its SHA-256 when the
plugin gave one, unpacks everything into a staging directory, and only then moves it into place
under `--base` (default: the working directory). A destination that already holds files is
refused unless `--force` is given. `--force` merges: files of the same path are replaced,
everything else is left, nothing is deleted. A failed download or a bad archive changes nothing.
Archive entries that would land outside their destination (`..`, absolute paths) are refused,
and so is any symbolic link that, followed for real, leads outside it or nowhere. Each
destination is checked again just before it is filled, so an earlier archive cannot redirect a
later one. A download is given up after a minute without an answer, half an hour in all, or
4 GiB; an archive may unpack to at most 16 GiB. A redirect from `https` to plain `http` is
refused. A staging directory left in the base by an interrupted run (`.speedyspec-staging-*`)
is removed by the next install there, so do not run two installs into one base at once. On success the command prints one line per destination,
`<destination>\t<absolute path>`.

`app install` refuses to run if a plugin hit a fatal error while loading, and it installs
nothing when a plugin's answer on an install hook does not match its contract; the message
names the plugin, the hook and the field.

A plugin that installs something registers an app and answers the driver:

```php
<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\ZipSource;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App('wordpress', 'WordPress', [], ['zip']);

    return $apps;
});

Hook::add('speedyspec_driver_zip', static function (array $sources, InstallRequest $request): array
{
    if ($request->app === 'wordpress') {
        $sources[] = new ZipSource('https://wordpress.org/latest.zip', '.', stripComponents: 1);
    }

    return $sources;
}, 10, 2);
```

`ZipSource` is `url` (https), `destination` (relative to the base; `.` is the base itself),
`sha256` (optional, 64 hex characters) and `stripComponents` (leading path segments to drop,
0 to 32). That plugin is `examples/install-wordpress`.

## Installing into Kubernetes

```
speedyspec app install <app> --driver kubernetes --namespace <ns> [--name <install>] [--timeout 10m] [--force-conflicts]
                             [--server <url> (--token-file <path|-> | SPEEDYSPEC_KUBE_TOKEN=...) [--ca-cert <pem> | --insecure-skip-tls-verify]]
                             [--context <kubeconfig context>]
speedyspec app uninstall <install> --namespace <ns> [--yes] [the same connection flags]
```

No kubectl or Helm is needed. The cluster is named with `--server` and a token, or, when
neither is given, taken from your kubeconfig (`--context` picks a context). The token is never a
flag value: put it in `SPEEDYSPEC_KUBE_TOKEN`, or in a file named by `--token-file` (`-` reads
it from standard input). It is never printed, recorded, or shown to plugins.

An install, in order:

1. The plugin's answer is checked. Nothing in the cluster changes if it is wrong.
2. Each resource is looked up. One that is already in the cluster and was not put there by an
   earlier install under this name stops the install, naming it: an install never takes over
   what it did not create (it would be deleted with the install). That covers a `Namespace`
   the plugin returns for a namespace that already exists, and a second install whose
   resources have the same names as the first.
3. `--namespace`, and any other namespace a resource names, is created if it is missing.
4. The install record is written: a Secret `speedyspec-install-<install>` (type
   `speedyspec.com/install`) in `--namespace`, listing everything about to be applied. It is
   written first so that an install that fails part-way can still be removed.
5. Each resource is applied in the order the plugin gave, with server-side apply (field manager
   `speedyspec`) and the labels `app.kubernetes.io/managed-by=speedyspec`,
   `speedyspec.com/app=<app>` and `speedyspec.com/install=<install>`. A resource that names no
   namespace gets `--namespace`. A CustomResourceDefinition may be followed by resources of
   the kind it defines: the definition is applied first and the cluster is given up to 30
   seconds to start serving the kind.
6. The install waits, up to `--timeout`, for its workloads: every Deployment, StatefulSet,
   DaemonSet and Job, or what the plugin's `waitFor` names. A Deployment is ready when every
   replica is of the new version and available and no old one is left; one whose rollout has
   stopped (`ProgressDeadlineExceeded`) fails the install at once.
7. The record is marked `installed`.

A request the cluster does not answer within 30 seconds is given up and reported, so a
connection that goes silent cannot hold the command past `--timeout` for long.

`<install>` defaults to the app's id; give `--name` to install the same app twice (the plugin must then
name its resources differently for each, or they go in different namespaces). Installing
again under the same name re-applies. A resource the plugin no longer returns is left running,
reported, and kept in the record so that uninstall removes it. On failure the command says
which resource and why, marks the record `failed`, and leaves what was applied in place:
nothing is rolled back. A field another manager owns (someone ran `kubectl edit`, a controller
writes it) fails the install naming the field and the manager; `--force-conflicts` takes it
over.

`app uninstall` reads the record, shows what will go and asks to confirm (`--yes` skips the
question; it warns when PersistentVolumeClaims, and so data, are among it). It deletes the
recorded resources in reverse order, then the namespaces the install created, then the record.
It runs no plugin code, so it works with `--no-plugins`. It leaves alone, and reports, anything
that no longer carries this install's label (something else replaced it), a namespace that
still holds such a thing, and any namespace the install did not create. If a delete fails the
record stays, and the command can be run again.

A plugin answers `speedyspec_driver_kubernetes` with a `KubernetesResources`: complete
manifests as arrays, in the order to apply them.

```php
<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\KubernetesResources;
use SpeedySpec\Cli\Contracts\WaitFor;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_driver_kubernetes', static function (mixed $given, InstallRequest $request): mixed
{
    if ($request->app !== 'demo') {
        return $given;
    }
    $resources = [
        ['apiVersion' => 'v1', 'kind' => 'ConfigMap', 'metadata' => ['name' => 'demo'], 'data' => new stdClass()],
        // ... a Deployment, a Service ...
    ];

    return new KubernetesResources($resources, [new WaitFor('Deployment', 'demo')]);
}, 10, 2);
```

Arrays become JSON as `json_encode` would make them: a list (`[]` included) is a JSON array and
any other array an object, so write an empty object as `new stdClass`
(`'emptyDir' => new stdClass`), not `[]`. The filter starts from `null`. A plugin with nothing
to add returns what it was given; one that adds to another plugin's answer builds a new
`KubernetesResources` from it (the class is readonly):
`new KubernetesResources([...$given->resources, $mine], $given->waitFor)`.

`waitFor` omitted waits on the default workloads; `[]` waits on nothing. Readiness is built in
for Deployment, StatefulSet, DaemonSet, Job and Pod; a `WaitFor` for any other kind must name
a status condition that has to be `True` (`new WaitFor('Widget', 'w', condition: 'Available')`).
The example plugin is `examples/install-demo-kubernetes`.

## Installing and removing plugins

```
speedyspec plugin install <https://... zip URL | owner/repo | owner/repo@tag> [--yes]
speedyspec plugin remove <name> [--yes]
```

`owner/repo` is a GitHub release (the latest, or the tag given): the release asset named
`speedyspec-plugin.zip` if there is one, otherwise the release's source archive. `GITHUB_TOKEN`,
when set, is sent to `api.github.com` only. Before downloading, the command shows what it will
fetch and warns that the plugin will run as you, with full access to your files and network; it
asks for confirmation, or needs `--yes` when it cannot ask. The archive must hold a
`speedyspec-plugin.json` (at its top, or inside its single top-level directory) that validates
and whose `contracts` range this binary satisfies. Plugins are stored in the user plugin
directory as `<vendor>.<name>`; an installed plugin of the same name is replaced only once the
new one has validated. Both commands work when an installed plugin is broken, and with
`--no-plugins`. Only plugins in the user plugin directory can be removed. The name must be a plugin name
(`vendor/package`); anything else is refused before any path is looked at.

## Updating speedyspec

```
speedyspec self update --check    # say whether a newer release exists; changes nothing
speedyspec self update            # download it, check it, and put it in place of this binary
```

`self update` asks GitHub for the latest release of `SpeedySpec/speedyspec-cli` and downloads the
binary for this machine (`speedyspec-linux-x86_64`, `speedyspec-linux-aarch64`,
`speedyspec-macos-x86_64` or `speedyspec-macos-aarch64`). Before anything changes it checks that
the download is no larger than the release says, that its SHA-256 matches the release's
`SHA256SUMS`, and that it runs and reports the release's version. Only then is it renamed over
the running binary, in one step; when you run speedyspec through a symlink, the file the link
points to is replaced and the link is left as it is. If any check fails, nothing is changed.

The binary's directory must be writable by you. When it is not (`/usr/local/bin`, say), the
command says so before downloading anything: run `sudo speedyspec self update`, or download the
binary yourself from
`https://github.com/SpeedySpec/speedyspec-cli/releases/latest/download/<binary name>`.

No credentials are needed. GitHub limits anonymous API calls; if it refuses, set `GITHUB_TOKEN`
(sent to `api.github.com` only). `self update` starts no PHP and loads no plugin, so a broken
plugin cannot stop you from getting a fixed binary. A release whose binaries are still being
uploaded (in the minutes after it is published) is reported as such; try again shortly.

## Hooks fired in this version

| Hook | Kind | Purpose |
| --- | --- | --- |
| `speedyspec_init` | action | Fired once, after every plugin has loaded. |
| `speedyspec_commands` | filter | Receives `[]`; append `Command` objects. |
| `speedyspec_apps` | filter | Receives `[]`; append `App` objects. |
| `speedyspec_install_options` | filter | Receives `[]`, the app id and the driver id; append `InstallOption` objects. Register with 3 accepted arguments. |
| `speedyspec_install_before` | action | Receives the `InstallRequest`. Throwing stops the install; nothing is installed. |
| `speedyspec_driver_zip` | filter | Receives `[]` and the `InstallRequest`; append `ZipSource` objects. Register with 2 accepted arguments. |
| `speedyspec_driver_kubernetes` | filter | Receives `null` (or an earlier plugin's answer) and the `InstallRequest`; return a `KubernetesResources`. Register with 2 accepted arguments. |
| `speedyspec_install_after` | action | Receives the `InstallRequest` and the result. For zip: a list of `['url' => ..., 'destination' => ..., 'path' => ...]`. For kubernetes: a list of `['apiVersion' => ..., 'kind' => ..., 'namespace' => ..., 'name' => ...]`. Register with 2 accepted arguments. |

The `InstallRequest` carries `app`, `parts` (the selected part ids), `driver`, `options` (the
install option values by name) and `settings` (for zip: `base`, the absolute base directory;
for kubernetes: `namespace` and `name`, never the server or the token).
What a plugin prints from the before and after actions is command output; what it prints from
the filters goes to stderr.

`speedyspec_commands` and `speedyspec_apps` must return a list of those objects. Each callback's
return value is checked as it comes back and every problem is reported against the plugin whose
callback returned it, as `speedyspec: <plugin>: <hook>: <field>: <problem>` (for example
`[1].id: must match ...`). A bad item is dropped on its own; a value that is not a list is
dropped whole and the list as it was before that callback continues to the next one, so other
plugins' commands and apps are unaffected. A callback added from inside another callback (say
from `speedyspec_init`) belongs to the plugin that added it.
`Hook::add` and WordPress-style `add_filter` / `add_action` register to the same store.
