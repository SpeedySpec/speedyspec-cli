<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\KubernetesResources;
use SpeedySpec\Cli\Contracts\WaitFor;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App('odd', 'Odd', [], ['kubernetes']);

    return $apps;
});

Hook::add('speedyspec_driver_kubernetes', static function (mixed $given, InstallRequest $request): mixed
{
    if ($request->app !== 'odd') {
        return $given;
    }
    $widget = ['apiVersion' => 'example.test/v1', 'kind' => 'Widget', 'metadata' => ['name' => 'w']];

    return new KubernetesResources([$widget], [new WaitFor('Widget', 'w')]);
}, 10, 2);
