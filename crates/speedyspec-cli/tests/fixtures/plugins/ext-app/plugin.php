<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\AppRuntime;
use SpeedySpec\Cli\Contracts\Extension;
use SpeedySpec\Cli\Contracts\ExtensionLayout;
use SpeedySpec\Cli\Contracts\Install;
use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\MaintenanceFile;
use SpeedySpec\Cli\Contracts\ZipSource;
use SpeedySpec\Hooks\Hook;

/*
 * An app whose plugins and themes live in ext/plugins and ext/themes. A plugin or theme directory holds info.txt
 * ("Name: …" and "Version: …" lines); a single-file plugin is a .php file with the same lines in a comment.
 */

Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App('ext-site', 'Extension site', [], ['zip']);

    return $apps;
});

Hook::add('speedyspec_driver_zip', static function (array $sources, InstallRequest $request): array
{
    if ($request->app === 'ext-site')
    {
        $sources[] = new ZipSource(getenv('ZIP_FIXTURE_URL') . '/web.zip', '.', stripComponents: 1);
    }

    return $sources;
}, 10, 2);

Hook::add('speedyspec_app_extensions', static function (?ExtensionLayout $layout, Install $install): ?ExtensionLayout
{
    if ($install->app !== 'ext-site')
    {
        return $layout;
    }
    $directories = getenv('EXT_ONLY_PLUGINS') === '1'
        ? ['plugin' => 'ext/plugins']
        : ['plugin' => 'ext/plugins', 'theme' => 'ext/themes'];

    return new ExtensionLayout($directories, static function (string $kind, string $path): ?Extension
    {
        // With EXT_MAINT_LOG: whether the site is in maintenance each time an entry is read.
        if (getenv('EXT_MAINT_LOG') !== false)
        {
            $maintenance = is_file(dirname($path, 3) . '/.maintenance') ? 'maintenance: on' : 'maintenance: off';
            file_put_contents(getenv('EXT_MAINT_LOG'), "$maintenance\n", FILE_APPEND);
        }
        $file = is_dir($path) ? $path . '/info.txt' : $path;
        if (!is_file($file) || (!is_dir($path) && !str_ends_with($path, '.php')))
        {
            return null;
        }
        $text = (string) file_get_contents($file);
        if (!preg_match('/Name: (.+)/', $text, $name))
        {
            return null;
        }
        $version = preg_match('/Version: (\S+)/', $text, $v) ? $v[1] : null;

        return new Extension($kind, basename($path, '.php'), trim($name[1]), $version);
    });
}, 10, 2);

// With EXT_MAINTENANCE: the site shows a maintenance page while its plugins and themes are swapped.
Hook::add('speedyspec_app_runtime', static function (?AppRuntime $runtime, Install $install): ?AppRuntime
{
    if ($install->app !== 'ext-site' || getenv('EXT_MAINTENANCE') !== '1')
    {
        return $runtime;
    }

    return new AppRuntime(maintenance: new MaintenanceFile('.maintenance', '<?php $upgrading = 1;'));
}, 10, 2);
