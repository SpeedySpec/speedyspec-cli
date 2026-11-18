# speedyspec

`speedyspec` is a command line with a statically linked PHP 8.5 interpreter inside it. It knows
nothing about any particular application: PHP plugins, loaded at start-up, add subcommands and
installable "apps" through hooks. No PHP, Composer or `php.ini` is needed on the machine.

```
speedyspec --version              # speedyspec, php and contracts versions, three lines
speedyspec php <file> [args...]   # run a PHP file with hooks available; exit code is the script's
speedyspec app list               # id <TAB> label <TAB> drivers ("?" marks a driver not implemented yet)
speedyspec plugin list            # name <TAB> version <TAB> directory
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

## Hooks fired in this version

| Hook | Kind | Purpose |
| --- | --- | --- |
| `speedyspec_init` | action | Fired once, after every plugin has loaded. |
| `speedyspec_commands` | filter | Receives `[]`; append `Command` objects. |
| `speedyspec_apps` | filter | Receives `[]`; append `App` objects. |

`speedyspec_commands` and `speedyspec_apps` must return a list of those objects. Each callback's
return value is checked as it comes back and every problem is reported against the plugin whose
callback returned it, as `speedyspec: <plugin>: <hook>: <field>: <problem>` (for example
`[1].id: must match ...`). A bad item is dropped on its own; a value that is not a list is
dropped whole and the list as it was before that callback continues to the next one, so other
plugins' commands and apps are unaffected. A callback added from inside another callback (say
from `speedyspec_init`) belongs to the plugin that added it.
`Hook::add` and WordPress-style `add_filter` / `add_action` register to the same store.
