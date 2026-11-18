<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\KubernetesResources;
use SpeedySpec\Hooks\Hook;

/**
 * Offers a small demo app the CLI can install into a Kubernetes cluster.
 *
 * It exists to show the shape of a Kubernetes install plugin, so the workload is the smallest
 * one there is: a container that does nothing.
 */
Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App('demo', 'Demo', [], ['kubernetes']);

    return $apps;
});

/**
 * Tells the Kubernetes driver what the demo app consists of.
 *
 * The resources are complete manifests written as arrays. None names a namespace, so each
 * takes the one the user chose; the CLI adds its own labels, applies them in this order and
 * waits for the Deployment.
 *
 * @param mixed $given
 *   What an earlier plugin returned for this install, or null.
 */
Hook::add('speedyspec_driver_kubernetes', static function (mixed $given, InstallRequest $request): mixed
{
    if ($request->app !== 'demo') {
        return $given;
    }
    $labels = ['app' => 'demo'];

    return new KubernetesResources([
        [
            'apiVersion' => 'v1',
            'kind' => 'ConfigMap',
            'metadata' => ['name' => 'demo'],
            'data' => ['greeting' => 'hello'],
        ],
        [
            'apiVersion' => 'apps/v1',
            'kind' => 'Deployment',
            'metadata' => ['name' => 'demo'],
            'spec' => [
                'replicas' => 1,
                'selector' => ['matchLabels' => $labels],
                'template' => [
                    'metadata' => ['labels' => $labels],
                    'spec' => [
                        'containers' => [['name' => 'pause', 'image' => 'registry.k8s.io/pause:3.10']],
                    ],
                ],
            ],
        ],
        [
            'apiVersion' => 'v1',
            'kind' => 'Service',
            'metadata' => ['name' => 'demo'],
            'spec' => ['selector' => $labels, 'ports' => [['port' => 80]]],
        ],
    ]);
}, 10, 2);
