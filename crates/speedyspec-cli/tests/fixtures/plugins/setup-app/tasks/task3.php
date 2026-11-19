<?php

declare(strict_types=1);

return static function (array $input): ?string
{
    file_put_contents('order.txt', ' 3', FILE_APPEND);

    return null;
};
