<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\AppPart;
use SpeedySpec\Cli\Contracts\InstallOption;
use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\ZipSource;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App(
        'site',
        'Site',
        [new AppPart('web', 'Web'), new AppPart('docs', 'Docs', false)],
        ['zip'],
    );

    return $apps;
});

Hook::add('speedyspec_install_options', static function (array $options, string $app, string $driver): array
{
    if ($app !== 'site') {
        return $options;
    }
    $options[] = new InstallOption('title', 'Site title', required: true);
    $options[] = new InstallOption('token', 'Deploy token', secret: true);

    return $options;
}, 10, 3);

Hook::add('speedyspec_install_before', static function (InstallRequest $request): void
{
    if ($request->app === 'site') {
        echo 'before:', $request->app, "\n";
    }
});

Hook::add('speedyspec_driver_zip', static function (array $sources, InstallRequest $request): array
{
    if ($request->app !== 'site') {
        return $sources;
    }
    $url = getenv('ZIP_FIXTURE_URL');
    if (in_array('web', $request->parts, true)) {
        $sources[] = new ZipSource($url . '/web.zip', '.', stripComponents: 1);
    }
    if (in_array('docs', $request->parts, true)) {
        $sources[] = new ZipSource($url . '/docs.zip', 'docs');
    }

    return $sources;
}, 10, 2);

Hook::add('speedyspec_install_after', static function (InstallRequest $request, array $placements): void
{
    if ($request->app === 'site') {
        echo 'after:', $request->options['title'], ' ', count($placements), ' ',
            strlen($request->options['token'] ?? ''), "\n";
    }
}, 10, 2);
