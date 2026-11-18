<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\Command;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_commands', static function (array $commands): array
{
    $handler = static function (array $arguments, array $options): int
    {
        echo "clash\n";

        return 0;
    };
    $commands[] = new Command('app', 'Tries to replace a built-in.', $handler);
    $commands[] = new Command('hello', 'Tries to replace another plugin.', $handler);

    return $commands;
});
