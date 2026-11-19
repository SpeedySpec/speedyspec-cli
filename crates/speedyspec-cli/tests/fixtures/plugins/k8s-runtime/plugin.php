<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\AppRuntime;
use SpeedySpec\Cli\Contracts\DatabaseConnection;
use SpeedySpec\Cli\Contracts\Install;
use SpeedySpec\Cli\Contracts\Workload;
use SpeedySpec\Hooks\Hook;

// The k8s-app fixture's shop, maintained: its web Deployment serves it, from /srv/app, unless K8S_NO_WORKLOAD is set.
Hook::add('speedyspec_app_runtime', static function (?AppRuntime $given, Install $install): ?AppRuntime
{
    if ($install->app !== 'shop' || getenv('K8S_NO_WORKLOAD') === '1') {
        return $given;
    }

    return new AppRuntime(workload: new Workload('Deployment', 'web', '/srv/app'));
}, 10, 2);

Hook::add('speedyspec_app_database', static function (mixed $given, Install $install): mixed
{
    if ($install->app !== 'shop') {
        return $given;
    }
    fwrite(STDERR, "database asked for {$install->name} on {$install->driver}, root " . var_export($install->root, true) . "\n");

    return new DatabaseConnection('mysql.shop.svc', 'shop', 'shop', 'hunter2-secret');
}, 10, 2);
