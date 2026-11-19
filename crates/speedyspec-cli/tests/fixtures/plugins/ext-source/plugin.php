<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\ExtensionArchive;
use SpeedySpec\Cli\Contracts\ExtensionSource;
use SpeedySpec\Hooks\Hook;

/*
 * A source over the test's server: <base>/<kind>/<slug>.<version>.zip. The latest version is EXT_LATEST (2.0 by
 * default). Slugs starting with "unknown" are not here; the slug "liar" answers for another slug.
 */

Hook::add('speedyspec_extension_sources', static function (array $sources): array
{
    $sources[] = new ExtensionSource('test', static function (string $kind, string $slug, ?string $version): ?ExtensionArchive
    {
        if (getenv('EXT_RESOLVE_LOG'))
        {
            file_put_contents((string) getenv('EXT_RESOLVE_LOG'), $slug . "\n", FILE_APPEND);
        }
        if (str_starts_with($slug, 'unknown'))
        {
            return null;
        }
        $version ??= getenv('EXT_LATEST') ?: '2.0';
        $sha = getenv('EXT_SHA256') ?: null;
        $answered = $slug === 'liar' ? 'someone-else' : $slug;

        return new ExtensionArchive(
            $kind,
            $answered,
            $version,
            getenv('EXT_SOURCE_URL') . "/{$kind}/{$slug}.{$version}.zip",
            $sha,
        );
    });

    return $sources;
});
