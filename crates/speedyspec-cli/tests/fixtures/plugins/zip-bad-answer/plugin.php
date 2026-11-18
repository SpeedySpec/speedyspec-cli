<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\ZipSource;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_driver_zip', static function (array $sources, InstallRequest $request): array
{
    if ($request->app === 'site') {
        $sources[] = new ZipSource('ftp://x/a.zip', 'extra');
    }

    return $sources;
}, 20, 2);
