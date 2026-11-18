<?php

declare(strict_types=1);

use SpeedySpec\Hooks\Hook;

// Each of the nine global aliases against the Hook method it stands for, on
// one registry: what one adds, the other sees and can remove.
$cb = static fn (mixed $value = null): mixed => $value;

// add_action / has_action / remove_action, and Hook::remove returning false.
add_action('alias.action', $cb);
echo Hook::has('alias.action', $cb) ? 'y' : 'n';
echo Hook::remove('alias.action', $cb, 99) ? 't' : 'f';
echo remove_action('alias.action', $cb) ? 't' : 'f';
echo has_action('alias.action') ? 'y' : 'n';
echo Hook::remove('alias.action', $cb) ? 't' : 'f';
echo '|';

// add_filter / apply_filters against Hook::add / Hook::filter.
add_filter('alias.filter', static fn (int $value): int => $value * 2);
Hook::add('alias.filter', static fn (int $value): int => $value + 1, 20);
echo apply_filters('alias.filter', 2), ',', Hook::filter('alias.filter', 3);
echo has_filter('alias.filter') ? 'y' : 'n';
echo '|';

// Hook::add seen and removed by has_filter / remove_filter.
Hook::add('alias.other', $cb);
echo has_filter('alias.other', $cb) ? 'y' : 'n';
echo remove_filter('alias.other', $cb) ? 't' : 'f';
echo Hook::has('alias.other') ? 'y' : 'n';
echo '|';

// do_action / Hook::run share one count, read by did_action / Hook::ran.
Hook::add('alias.run', static function (): void
{
    echo 'r';
});
do_action('alias.run');
Hook::run('alias.run');
echo did_action('alias.run'), Hook::ran('alias.run');
