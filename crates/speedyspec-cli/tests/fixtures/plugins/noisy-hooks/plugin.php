<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_init', static function (): void
{
    echo "noise from init\n";
});

Hook::add('speedyspec_commands', static function (array $commands): array
{
    echo "noise from commands\n";

    return $commands;
});

Hook::add('speedyspec_apps', static function (array $apps): array
{
    echo "noise from apps\n";
    $apps[] = new App('quiet', 'Quiet', [], ['zip']);

    return $apps;
});
