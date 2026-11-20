<?php

declare(strict_types=1);

// Fails once while a file fail-two is in the app's root (and removes it), then succeeds.
return static function (array $input): string
{
    if (is_file(getcwd() . '/fail-two')) {
        unlink(getcwd() . '/fail-two');
        throw new RuntimeException('two failed this time');
    }

    return 'two done';
};
