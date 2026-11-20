<?php

declare(strict_types=1);

// Writes the secret it is given into the app's volume, as a config writer would, and says so.
return static function (array $input): string
{
    file_put_contents(getcwd() . '/config.txt', 'secret=' . $input['password'] . "\n");

    return 'wrote config.txt in ' . getcwd();
};
