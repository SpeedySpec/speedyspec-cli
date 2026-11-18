<?php

declare(strict_types=1);

use SpeedySpec\Hooks\Hook;

// PHP_INT_MAX runs last and PHP_INT_MIN first, the WordPress idiom.
Hook::add('extreme.order', static fn () => print('c'), PHP_INT_MAX);
Hook::add('extreme.order', static fn () => print('b'));
Hook::add('extreme.order', static fn () => print('a'), PHP_INT_MIN);
Hook::run('extreme.order');
echo '|';

// Removal needs the same extreme priority, as an int or a numeric string.
$last = static fn () => print('x');
add_action('extreme.remove', $last, PHP_INT_MAX);
echo remove_action('extreme.remove', $last) ? '1' : '0';
echo remove_action('extreme.remove', $last, PHP_INT_MAX) ? '1' : '0';
$first = static fn () => print('y');
add_filter('extreme.remove', $first, PHP_INT_MIN);
echo remove_filter('extreme.remove', $first, (string) PHP_INT_MIN) ? '1' : '0';
Hook::run('extreme.remove');
echo Hook::has('extreme.remove') ? 'y' : 'n';
