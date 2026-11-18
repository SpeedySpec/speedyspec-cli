<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\AppTask;
use SpeedySpec\Cli\Contracts\Install;
use SpeedySpec\Hooks\Hook;

/*
 * A fake app shaped like WordPress: its database settings are constants in config.php, which
 * ends by requiring ABSPATH . 'settings.php'. The settings are read by running config.php in the
 * runner with ABSPATH pointing at an empty stub, the way a WordPress plugin reads wp-config.php.
 */

Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App('config-app', 'Config app', [], ['zip']);

    return $apps;
});

Hook::add('speedyspec_app_database', static function (?object $answer, Install $install): ?object
{
    if ($install->app !== 'config-app')
    {
        return $answer;
    }

    return new AppTask(__DIR__ . '/read-config.php', ['root' => $install->root], loadsApp: false);
}, 10, 2);
