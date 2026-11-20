<?php

declare(strict_types=1);

// Fails once while fail-second is in the site's files (and removes it), then succeeds.
return static function (array $input): string
{
    if (is_file('fail-second')) {
        unlink('fail-second');
        throw new RuntimeException('the second task failed this time');
    }
    file_put_contents('order.txt', 'second', FILE_APPEND);

    return 'second done';
};
