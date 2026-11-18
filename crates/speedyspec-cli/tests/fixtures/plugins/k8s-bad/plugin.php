<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\KubernetesResources;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_driver_kubernetes', static function (mixed $given, InstallRequest $request): mixed
{
    if ($request->app !== 'shop' || ! $given instanceof KubernetesResources) {
        return $given;
    }
    $nameless = ['apiVersion' => 'v1', 'kind' => 'ConfigMap', 'metadata' => ['labels' => ['a' => 'b']]];

    return new KubernetesResources([$given->resources[0], $nameless]);
}, 20, 2);
