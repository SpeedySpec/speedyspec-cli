<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\KubernetesResources;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App('maps', 'Maps', [], ['kubernetes']);

    return $apps;
});

Hook::add('speedyspec_driver_kubernetes', static function (mixed $given, InstallRequest $request): mixed
{
    if ($request->app !== 'maps') {
        return $given;
    }

    // `new stdClass` is an empty object ({}); `[]` is an empty list, as json_encode has it.
    return new KubernetesResources([
        ['apiVersion' => 'v1', 'kind' => 'ConfigMap', 'metadata' => ['name' => 'empty'], 'data' => new stdClass()],
        [
            'apiVersion' => 'v1',
            'kind' => 'Pod',
            'metadata' => ['name' => 'scratch'],
            'spec' => [
                'containers' => [['name' => 'pause', 'image' => 'registry.k8s.io/pause:3.10', 'args' => []]],
                'volumes' => [['name' => 'tmp', 'emptyDir' => new stdClass()]],
            ],
        ],
    ], []);
}, 10, 2);
