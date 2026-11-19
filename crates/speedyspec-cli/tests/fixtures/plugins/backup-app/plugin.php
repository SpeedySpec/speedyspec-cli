<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\App;
use SpeedySpec\Cli\Contracts\BackupSpec;
use SpeedySpec\Cli\Contracts\DatabaseConnection;
use SpeedySpec\Cli\Contracts\Install;
use SpeedySpec\Cli\Contracts\PrefixedColumn;
use SpeedySpec\Cli\Contracts\SiteUrl;
use SpeedySpec\Cli\Contracts\ZipSource;
use SpeedySpec\Hooks\Hook;

/*
 * A fake app whose database settings are in db.json at its root (kept from the target on restore, as an app's
 * configuration is), whose site URL is the `siteurl` row of `{prefix}options`, and whose `option_name` values may
 * carry the prefix.
 */

Hook::add('speedyspec_apps', static function (array $apps): array
{
    $apps[] = new App('backup-app', 'Backup app', [], ['zip']);

    return $apps;
});

Hook::add('speedyspec_driver_zip', static function (array $sources): array
{
    $sources[] = new ZipSource((string) getenv('BACKUP_APP_ARCHIVE_URL'), '.');

    return $sources;
});

Hook::add('speedyspec_app_database', static function (?object $answer, Install $install): ?object
{
    if ($install->app !== 'backup-app')
    {
        return $answer;
    }
    $settings = json_decode((string) file_get_contents($install->root . '/db.json'), true, 512, JSON_THROW_ON_ERROR);

    return new DatabaseConnection(
        $settings['host'],
        $settings['database'],
        $settings['user'],
        $settings['password'],
        port: $settings['port'] ?? 3306,
        tablePrefix: $settings['tablePrefix'] ?? '',
    );
}, 10, 2);

Hook::add('speedyspec_app_backup', static function (?object $answer, Install $install): ?object
{
    if ($install->app !== 'backup-app')
    {
        return $answer;
    }

    return new BackupSpec(
        ['.'],
        ['cache'],
        ['db.json'],
        new SiteUrl('options', 'option_name', 'siteurl', 'option_value'),
        [new PrefixedColumn('options', 'option_name')],
        '1.0.0',
    );
}, 10, 2);
