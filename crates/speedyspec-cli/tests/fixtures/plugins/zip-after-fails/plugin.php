<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_install_after', static function (InstallRequest $request, array $placements): void
{
    throw new RuntimeException('post step failed');
}, 20, 2);
