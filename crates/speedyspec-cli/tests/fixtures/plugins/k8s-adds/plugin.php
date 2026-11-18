<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\KubernetesResources;
use SpeedySpec\Hooks\Hook;

// Adds to another plugin's answer. The class is readonly, so a new one is built from the given.
Hook::add('speedyspec_driver_kubernetes', static function (mixed $given, InstallRequest $request): mixed
{
    if ($request->app !== 'shop' || ! $given instanceof KubernetesResources) {
        return $given;
    }
    $service = [
        'apiVersion' => 'v1',
        'kind' => 'Service',
        'metadata' => ['name' => 'web'],
        'spec' => ['selector' => ['app' => 'web'], 'ports' => [['port' => 80]]],
    ];

    return new KubernetesResources([...$given->resources, $service], $given->waitFor);
}, 20, 2);
