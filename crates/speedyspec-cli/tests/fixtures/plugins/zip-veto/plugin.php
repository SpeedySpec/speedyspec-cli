<?php

declare(strict_types=1);

use SpeedySpec\Cli\Contracts\InstallRequest;
use SpeedySpec\Hooks\Hook;

Hook::add('speedyspec_install_before', static function (InstallRequest $request): void
{
    throw new RuntimeException('not allowed');
});
