<?php

declare(strict_types=1);

use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_apps', static fn (array $apps): string => 'nope');
