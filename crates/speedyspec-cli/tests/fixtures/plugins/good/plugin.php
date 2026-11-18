<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\AppPart;
use SpeedySpec\Cli\Contracts\Command;
use SpeedySpec\Cli\Contracts\CommandArgument;
use SpeedySpec\Cli\Contracts\CommandOption;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App(
        'demo',
        'Demo',
        [new AppPart('web', 'Web'), new AppPart('worker', 'Worker')],
        ['zip'],
    );

    return $apps;
});

Hook::add('speedyspec_commands', static function (array $commands): array
{
    $commands[] = new Command(
        'hello',
        'Greets someone.',
        static function (array $arguments, array $options): int
        {
            $greeting = 'hello ' . $arguments['name'];
            echo $options['shout'] ? strtoupper($greeting) : $greeting, "\n";

            return 0;
        },
        [new CommandArgument('name', 'Who to greet.')],
        [new CommandOption('shout', 'Upper-case the greeting.', takesValue: false)],
    );

    return $commands;
});
