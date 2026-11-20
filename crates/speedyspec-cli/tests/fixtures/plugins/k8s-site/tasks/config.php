<?php

declare(strict_types=1);

// Writes the config, holding the secret, into the site's files; asked to, also leaves a file that fails the next task.
return static function (array $input): string
{
    file_put_contents('config.txt', 'secret=' . $input['password'] . "\n");
    if ($input['fail']) {
        touch('fail-second');
    }

    return 'config written in ' . getcwd();
};
