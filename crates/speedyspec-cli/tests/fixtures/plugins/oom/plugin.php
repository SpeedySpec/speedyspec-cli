<?php

declare(strict_types=1);

(static function (): void
{
    $chunks = [];
    while (true)
    {
        $chunks[] = str_repeat('x', 1000000);
    }
})();
