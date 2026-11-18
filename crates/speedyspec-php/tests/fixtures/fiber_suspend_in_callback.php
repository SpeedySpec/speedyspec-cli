<?php

declare(strict_types=1);

use SpeedySpec\Hooks\Hook;

Hook::add('fiber.suspends', static function (): void
{
    Fiber::suspend('from a hook callback');
});

$fiber = new Fiber(static function (): string
{
    try
    {
        Hook::run('fiber.suspends');
        return 'no error';
    }
    catch (FiberError $e)
    {
        return 'FiberError';
    }
});
$fiber->start();
echo $fiber->isTerminated() ? $fiber->getReturn() : 'suspended inside the hook';
