<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\Command;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_commands', static function (array $commands): array
{
    $commands[] = new Command(
        'grumble',
        'Emits a warning and a deprecation, then reports success.',
        static function (array $arguments, array $options): int
        {
            trigger_error('careful now', E_USER_WARNING);
            trigger_error('old way', E_USER_DEPRECATED);
            echo 'done';

            return 0;
        },
    );

    return $commands;
});
