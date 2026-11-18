<?php

declare(strict_types=1);

/*
 * Reads the app's database settings by running its config.php with nothing else of the app.
 */

return static function (array $input): array
{
    define('ABSPATH', __DIR__ . '/stub/');
    require $input['root'] . '/config.php';

    return [
        'host' => DB_HOST,
        'database' => DB_NAME,
        'user' => DB_USER,
        'password' => DB_PASSWORD,
        'port' => (int) DB_PORT,
        'tablePrefix' => $table_prefix ?? 'wp_',
    ];
};
