<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\AppPart;
use SpeedySpec\Cli\Contracts\InstallOption;
use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\KubernetesResources;
use SpeedySpec\Hooks\Hook;

/**
 * A one-replica Deployment of the pause image, as an array a plugin would write.
 */
function k8s_fixture_deployment(string $name): array
{
    return [
        'apiVersion' => 'apps/v1',
        'kind' => 'Deployment',
        'metadata' => ['name' => $name],
        'spec' => [
            'replicas' => 1,
            'selector' => ['matchLabels' => ['app' => $name]],
            'template' => [
                'metadata' => ['labels' => ['app' => $name]],
                'spec' => ['containers' => [['name' => $name, 'image' => 'registry.k8s.io/pause:3.10']]],
            ],
        ],
    ];
}

Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App(
        'shop',
        'Shop',
        [new AppPart('web', 'Web'), new AppPart('worker', 'Worker', false)],
        ['kubernetes'],
    );

    return $apps;
});

Hook::add('speedyspec_install_options', static function (array $options, string $app, string $driver): array
{
    if ($app === 'shop') {
        $options[] = new InstallOption('title', 'Shop title', required: true);
    }

    return $options;
}, 10, 3);

Hook::add('speedyspec_driver_kubernetes', static function (mixed $given, InstallRequest $request): mixed
{
    if ($request->app !== 'shop') {
        return $given;
    }
    $resources = [
        [
            'apiVersion' => 'v1',
            'kind' => 'ConfigMap',
            'metadata' => ['name' => 'settings'],
            'data' => ['title' => $request->options['title']],
        ],
        k8s_fixture_deployment('web'),
    ];
    if (in_array('worker', $request->parts, true)) {
        $resources[] = k8s_fixture_deployment('worker');
    }

    return new KubernetesResources($resources);
}, 10, 2);

Hook::add('speedyspec_install_after', static function (InstallRequest $request, array $resources): void
{
    if ($request->app === 'shop') {
        echo 'after:', count($resources), ' ', $request->settings['namespace'], ' ',
            $request->settings['name'], "\n";
        if (getenv('K8S_FIXTURE_DUMP_REQUEST') === '1') {
            echo json_encode($request), "\n";
        }
    }
}, 10, 2);
