<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\Command;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_commands', static function (array $commands): array
{
    $commands[] = new Command(
        'boom',
        'Throws from its handler.',
        static function (array $arguments, array $options): int
        {
            throw new RuntimeException('kaboom');
        },
    );

    return $commands;
});
