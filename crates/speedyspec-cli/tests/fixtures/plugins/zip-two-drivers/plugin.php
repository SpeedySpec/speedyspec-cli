<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\ZipSource;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App('multi', 'Multi', [], ['zip', 'kubernetes']);

    return $apps;
});

Hook::add('speedyspec_driver_zip', static function (array $sources, InstallRequest $request): array
{
    if ($request->app === 'multi') {
        $sources[] = new ZipSource(getenv('ZIP_FIXTURE_URL') . '/docs.zip', '.');
    }

    return $sources;
}, 10, 2);
