<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\Command;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_commands', static function (array $commands): array
{
    $commands[] = new Command(
        'crash',
        'Hits a real engine fatal error from its handler.',
        static function (array $arguments, array $options): int
        {
            eval('function declared_twice() {} function declared_twice() {}');

            return 0;
        },
    );

    return $commands;
});
