<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Cli\Contracts\ZipSource;
use SpeedySpec\Hooks\Hook;

/**
 * Offers WordPress as an app the CLI can install.
 *
 * WordPress ships as one archive, so the app has no parts and the zip driver is the only way in.
 */
Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App('wordpress', 'WordPress', [], ['zip']);

    return $apps;
});

/**
 * Tells the zip driver where WordPress is and where it goes.
 *
 * The official archive wraps everything in a `wordpress/` directory; dropping that one leading
 * segment puts the files straight into the base directory the user chose.
 */
Hook::add('speedyspec_driver_zip', static function (array $sources, InstallRequest $request): array
{
    if ($request->app === 'wordpress') {
        $sources[] = new ZipSource('https://wordpress.org/latest.zip', '.', stripComponents: 1);
    }

    return $sources;
}, 10, 2);
