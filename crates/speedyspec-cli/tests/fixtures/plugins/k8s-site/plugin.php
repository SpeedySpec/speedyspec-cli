<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\AppPart;
use SpeedySpec\Cli\Contracts\AppRuntime;
use SpeedySpec\Cli\Contracts\AppTask;
use SpeedySpec\Cli\Contracts\BackupSpec;
use SpeedySpec\Cli\Contracts\DatabaseConnection;
use SpeedySpec\Cli\Contracts\Extension;
use SpeedySpec\Cli\Contracts\ExtensionLayout;
use SpeedySpec\Cli\Contracts\Install;
use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\KubernetesResources;
use SpeedySpec\Cli\Contracts\MaintenanceFile;
use SpeedySpec\Cli\Contracts\SetupPlan;
use SpeedySpec\Cli\Contracts\SiteUrl;
use SpeedySpec\Cli\Contracts\WaitFor;
use SpeedySpec\Cli\Contracts\Workload;
use SpeedySpec\Cli\Contracts\ZipSource;
use SpeedySpec\Hooks\Hook;

// A site on a real cluster (the throwaway k3s): a busybox Deployment that serves nothing, with its files on a claim at
// /srv/app. Its setup writes a config holding a secret, as an app's installer would.
const K8S_SITE_IMAGE = 'busybox@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec';

Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App('site', 'Site', [new AppPart('web', 'Web')], ['kubernetes', 'zip']);

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

    return new AppRuntime(
        workload: $install->driver === 'kubernetes' ? new Workload('Deployment', $install->name, '/srv/app', 'web') : null,
        maintenance: new MaintenanceFile('.maintenance', '<?php $upgrading = 1;'),
    );
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

// The site's database: the mysql Service in its namespace, reached from inside the cluster. The test gives the
// password in K8S_SITE_DB_PASSWORD.
Hook::add('speedyspec_app_database', static function (mixed $given, Install $install): mixed
{
    if ($install->app !== 'site') {
        return $given;
    }

    if ($install->driver === 'zip') {
        // On this machine: the throwaway MySQL from scripts/mysql.sh, as K8S_SITE_ZIP_DB (mysql://user:pass@host:port).
        $url = parse_url((string) getenv('K8S_SITE_ZIP_DB'));

        return new DatabaseConnection(
            (string) $url['host'],
            'site_zip',
            (string) $url['user'],
            (string) $url['pass'],
            (int) $url['port'],
            tablePrefix: 'wp_',
        );
    }

    return new DatabaseConnection('mysql', 'site', 'root', (string) getenv('K8S_SITE_DB_PASSWORD'), tablePrefix: 'wp_');
}, 10, 2);

// Plugins in ext/plugins, each a directory with an info.txt of "Name: …" and "Version: …" lines.
Hook::add('speedyspec_app_extensions', static function (?ExtensionLayout $layout, Install $install): ?ExtensionLayout
{
    if ($install->app !== 'site') {
        return $layout;
    }

    return new ExtensionLayout(['plugin' => 'ext/plugins'], static function (string $kind, string $path): ?Extension
    {
        $file = $path . '/info.txt';
        if (!is_dir($path) || !is_file($file)) {
            return null;
        }
        $text = (string) file_get_contents($file);
        if (!preg_match('/Name: (.+)/', $text, $name)) {
            return null;
        }
        $version = preg_match('/Version: (\S+)/', $text, $v) ? $v[1] : null;

        return new Extension($kind, basename($path), trim($name[1]), $version);
    });
}, 10, 2);

// On this machine, the site's files come from K8S_SITE_ZIP_URL/site.zip.
Hook::add('speedyspec_driver_zip', static function (array $sources, InstallRequest $request): array
{
    if ($request->app === 'site') {
        $sources[] = new ZipSource(getenv('K8S_SITE_ZIP_URL') . '/site.zip', '.', stripComponents: 1);
    }

    return $sources;
}, 10, 2);

// A backup holds all of the site's files and its tables; its URL is in the options table.
Hook::add('speedyspec_app_backup', static function (?BackupSpec $spec, Install $install): ?BackupSpec
{
    if ($install->app !== 'site') {
        return $spec;
    }

    return new BackupSpec(['.'], [], [], new SiteUrl('options', 'option_name', 'siteurl', 'option_value'));
}, 10, 2);
