<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\Command;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_commands', static function (array $commands): array
{
    $commands[] = new Command(
        'wphooks',
        'Shows that the container and the native Hook API are present and WordPress\'s legacy names are not.',
        static function (array $arguments, array $options): int
        {
            $container = \SpeedySpec\Container\Container::getInstance();
            $container->bind('answer', static fn () => 42);
            echo implode(' ', [
                function_exists('add_filter') ? 'add_filter' : 'no',
                class_exists('WP_Hook', false) ? 'WP_Hook' : 'no',
                $container->make('answer'),
                function_exists('SpeedySpec\WpHooks\Native\add_filter') ? 'yes' : 'missing',
            ]), "\n";

            return 0;
        },
    );

    return $commands;
});
