<?php

declare(strict_types=1);

use SpeedySpec\Hooks\Hook;

/**
 * Throws when it is destroyed, so releasing a rejected return value raises an exception outside any call.
 */
final class DtorBadThrower
{
    public function __destruct()
    {
        throw new RuntimeException('thrower destructor');
    }
}

Hook::add('speedyspec_apps', static fn (array $apps): DtorBadThrower => new DtorBadThrower());
