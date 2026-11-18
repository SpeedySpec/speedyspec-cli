<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\InstallOption;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_install_options', static function (array $options, string $app, string $driver): array
{
    if ($app === 'site') {
        $options[] = new InstallOption('driver', 'Clashes with a built-in flag');
        $options[] = new InstallOption('title', 'Declared a second time');
    }

    return $options;
}, 20, 3);
