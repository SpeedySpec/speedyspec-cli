<?php

declare(strict_types=1);

return static function (array $input): string
{
    file_put_contents('order.txt', '1', FILE_APPEND);
    file_put_contents('secret.txt', $input['password']);

    return 'task one done';
};
