# speedyspec

`speedyspec` is a command line with a statically linked PHP 8.5 interpreter inside it. It knows
nothing about any particular application: PHP plugins, loaded at start-up, add subcommands and
installable "apps" through hooks. No PHP, Composer or `php.ini` is needed on the machine.

```
speedyspec --version              # speedyspec, php and contracts versions, three lines
speedyspec php <file> [args...]   # run a PHP file with hooks available; exit code is the script's
speedyspec app list               # id <TAB> label <TAB> drivers ("?" marks a driver this binary cannot run)
speedyspec app install <app> ...  # install an app a plugin provides (see "Installing an app")
speedyspec plugin list            # name <TAB> version <TAB> directory
speedyspec plugin install <src>   # install a plugin from a zip URL or a GitHub release
speedyspec plugin remove <name>   # remove an installed plugin
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

## Building

The PHP contracts the plugins program against are a separate package. `build.rs` embeds them
from a sibling checkout, `../speedyspec-cli-contracts` relative to this repository (override with
`SPEEDYSPEC_CONTRACTS_DIR`), and fails with a clear message if it is missing.

```
scripts/build-php.sh                # once: builds the static PHP into .php-build/ (slow)
cargo build --release -p speedyspec-cli
./target/release/speedyspec --version
cargo test --workspace
scripts/linux.sh cargo test --workspace   # the same, in a Linux (musl) container
```

See `docs/php-embed.md` for how PHP is built, linked and embedded.

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
only driver so far is `zip`. Anything missing is asked for when stdin is a terminal and
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
Archive entries that would land outside their destination (`..`, absolute paths, symbolic links
leading out) are refused. On success the command prints one line per destination,
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
`--no-plugins`. Only plugins in the user plugin directory can be removed.

## Hooks fired in this version

| Hook | Kind | Purpose |
| --- | --- | --- |
| `speedyspec_init` | action | Fired once, after every plugin has loaded. |
| `speedyspec_commands` | filter | Receives `[]`; append `Command` objects. |
| `speedyspec_apps` | filter | Receives `[]`; append `App` objects. |
| `speedyspec_install_options` | filter | Receives `[]`, the app id and the driver id; append `InstallOption` objects. Register with 3 accepted arguments. |
| `speedyspec_install_before` | action | Receives the `InstallRequest`. Throwing stops the install; nothing is installed. |
| `speedyspec_driver_zip` | filter | Receives `[]` and the `InstallRequest`; append `ZipSource` objects. Register with 2 accepted arguments. |
| `speedyspec_install_after` | action | Receives the `InstallRequest` and the result: for zip, a list of `['url' => ..., 'destination' => ..., 'path' => ...]`. Register with 2 accepted arguments. |

The `InstallRequest` carries `app`, `parts` (the selected part ids), `driver`, `options` (the
install option values by name) and `settings` (for zip: `base`, the absolute base directory).
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
