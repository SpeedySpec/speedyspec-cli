<?php

declare(strict_types=1);

use SpeedySpec\Hooks\Hook;

// The bad callback is registered only once every plugin has loaded, when no plugin is being
// loaded any more, so only the callback that adds it can say whose it is.
Hook::add('speedyspec_init', static function (): void
{
    Hook::add('speedyspec_apps', static fn (array $apps): string => 'late');
});
