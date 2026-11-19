<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\AppTask;
use SpeedySpec\Cli\Contracts\Install;
use SpeedySpec\Cli\Contracts\InstallOption;
use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\SetupPlan;
use SpeedySpec\Cli\Contracts\ZipSource;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App('setup-site', 'Setup site', [], ['zip']);

    return $apps;
});

Hook::add('speedyspec_install_options', static function (array $options, string $app): array
{
    if ($app === 'setup-site')
    {
        $options[] = new InstallOption('db-password', 'Database password', required: true, secret: true);
    }

    return $options;
}, 10, 2);

Hook::add('speedyspec_driver_zip', static function (array $sources, InstallRequest $request): array
{
    if ($request->app === 'setup-site')
    {
        $sources[] = new ZipSource(getenv('ZIP_FIXTURE_URL') . '/web.zip', '.', stripComponents: 1);
    }

    return $sources;
}, 10, 2);

Hook::add('speedyspec_app_setup', static function (?SetupPlan $plan, Install $install, InstallRequest $request): ?SetupPlan
{
    if ($install->app !== 'setup-site')
    {
        return $plan;
    }
    $input = ['password' => $request->options['db-password']];

    return new SetupPlan([
        new AppTask(__DIR__ . '/tasks/task1.php', $input, loadsApp: false),
        new AppTask(__DIR__ . '/tasks/task2.php', loadsApp: false),
        new AppTask(__DIR__ . '/tasks/task3.php', loadsApp: false),
    ]);
}, 10, 3);

Hook::add('speedyspec_install_after', static function (InstallRequest $request, array $placements): void
{
    if ($request->app === 'setup-site')
    {
        echo 'after: ', count($placements), "\n";
    }
}, 10, 2);
