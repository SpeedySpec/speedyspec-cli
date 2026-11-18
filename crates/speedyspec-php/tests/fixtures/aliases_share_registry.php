<?php

declare(strict_types=1);

use SpeedySpec\Hooks\Hook;

add_filter('aliases.filter', static fn (mixed $v): mixed => $v);
echo Hook::has('aliases.filter') ? 'y' : 'n';

add_action('aliases.action', static function (): void
{
});
do_action('aliases.action');
echo did_action('aliases.action');
