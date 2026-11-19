<?php

declare(strict_types=1);

return static function (array $input): ?string
{
    if (getenv('SETUP_FAIL_TASK2') === '1')
    {
        throw new RuntimeException('task two was told to fail');
    }
    file_put_contents('order.txt', ' 2', FILE_APPEND);

    return null;
};
