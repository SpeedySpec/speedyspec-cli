<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\Command;
use SpeedySpec\Cli\Contracts\CommandArgument;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_commands', static function (array $commands): array
{
    $commands[] = new Command(
        'status',
        'Returns the status it is given.',
        static function (array $arguments, array $options): int
        {
            return (int) $arguments['code'];
        },
        [new CommandArgument('code', 'The status to return.')],
    );

    return $commands;
});
