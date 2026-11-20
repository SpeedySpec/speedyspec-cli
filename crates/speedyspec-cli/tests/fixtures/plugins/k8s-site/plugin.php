<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\AppPart;
use SpeedySpec\Cli\Contracts\AppRuntime;
use SpeedySpec\Cli\Contracts\AppTask;
use SpeedySpec\Cli\Contracts\Install;
use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\KubernetesResources;
use SpeedySpec\Cli\Contracts\SetupPlan;
use SpeedySpec\Cli\Contracts\WaitFor;
use SpeedySpec\Cli\Contracts\Workload;
use SpeedySpec\Hooks\Hook;

// A site on a real cluster (the throwaway k3s): a busybox Deployment that serves nothing, with its files on a claim at
// /srv/app. Its setup writes a config holding a secret, as an app's installer would.
const K8S_SITE_IMAGE = 'busybox@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec';

Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App('site', 'Site', [new AppPart('web', 'Web')], ['kubernetes']);

    return $apps;
});

Hook::add('speedyspec_driver_kubernetes', static function (mixed $given, InstallRequest $request): mixed
{
    if ($request->app !== 'site') {
        return $given;
    }
    $name = $request->settings['name'];

    return new KubernetesResources([
        [
            'apiVersion' => 'v1',
            'kind' => 'PersistentVolumeClaim',
            'metadata' => ['name' => "$name-files"],
            'spec' => ['accessModes' => ['ReadWriteOnce'], 'resources' => ['requests' => ['storage' => '64Mi']]],
        ],
        [
            'apiVersion' => 'apps/v1',
            'kind' => 'Deployment',
            'metadata' => ['name' => $name],
            'spec' => [
                'replicas' => 1,
                'selector' => ['matchLabels' => ['app' => $name]],
                'template' => [
                    'metadata' => ['labels' => ['app' => $name]],
                    'spec' => [
                        'terminationGracePeriodSeconds' => 0,
                        'containers' => [[
                            'name' => 'web',
                            'image' => K8S_SITE_IMAGE,
                            'command' => ['sleep', '86400'],
                            'volumeMounts' => [['name' => 'files', 'mountPath' => '/srv/app']],
                        ]],
                        'volumes' => [['name' => 'files', 'persistentVolumeClaim' => ['claimName' => "$name-files"]]],
                    ],
                ],
            ],
        ],
    ], [new WaitFor('Deployment', $name)]);
}, 10, 2);

Hook::add('speedyspec_app_runtime', static function (?AppRuntime $given, Install $install): ?AppRuntime
{
    if ($install->app !== 'site') {
        return $given;
    }

    return new AppRuntime(workload: new Workload('Deployment', $install->name, '/srv/app', 'web'));
}, 10, 2);

Hook::add('speedyspec_app_setup', static function (?SetupPlan $plan, Install $install, InstallRequest $request): ?SetupPlan
{
    if ($install->app !== 'site') {
        return $plan;
    }

    return new SetupPlan([
        new AppTask(__DIR__ . '/tasks/config.php', ['password' => 'hunter2-secret', 'fail' => getenv('K8S_SITE_FAIL') === '1'], loadsApp: false),
        new AppTask(__DIR__ . '/tasks/second.php', loadsApp: false),
        new AppTask(__DIR__ . '/tasks/look.php', loadsApp: false),
    ]);
}, 10, 3);
