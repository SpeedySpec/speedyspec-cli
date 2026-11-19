<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\Command;
use SpeedySpec\Cli\Contracts\CommandArgument;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_commands', static function (array $commands): array
{
    $commands[] = new Command(
        'get',
        'Prints a URL\'s body through SpeedySpec\Cli\http_get, or ERR: and the message.',
        static function (array $arguments, array $options): int
        {
            try
            {
                $body = ($arguments['max'] ?? null) === null
                    ? \SpeedySpec\Cli\http_get($arguments['url'])
                    : \SpeedySpec\Cli\http_get($arguments['url'], (int) $arguments['max']);
                echo $body;
            }
            catch (\RuntimeException $e)
            {
                echo 'ERR: ', $e->getMessage();
            }

            return 0;
        },
        [new CommandArgument('url', 'The URL.'), new CommandArgument('max', 'The most bytes.', required: false)],
    );

    return $commands;
});
