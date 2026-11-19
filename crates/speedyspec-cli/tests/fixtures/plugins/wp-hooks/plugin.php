<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\Command;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_commands', static function (array $commands): array
{
    $commands[] = new Command(
        'wphooks',
        'Uses the WordPress hook functions, WP_Hook and the container.',
        static function (array $arguments, array $options): int
        {
            add_filter('fixture', static fn (string $v): string => $v . '-x');
            $container = \SpeedySpec\Container\Container::getInstance();
            $container->bind('answer', static fn () => 42);
            echo implode(' ', [
                function_exists('add_filter') ? 'add_filter' : 'missing',
                class_exists('WP_Hook') ? 'WP_Hook' : 'missing',
                $container->make('answer'),
                apply_filters('fixture', 'v'),
            ]), "\n";

            return 0;
        },
    );

    return $commands;
});
