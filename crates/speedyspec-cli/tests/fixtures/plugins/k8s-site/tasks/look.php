<?php

declare(strict_types=1);

// What is left of the request in the runner's private directory while a task runs (it should be nothing), after a
// pause long enough for the test to look at the Job.
return static function (array $input): string
{
    sleep(4);
    $left = glob(dirname(__DIR__, 2) . '/task.json-*') ?: [];

    return 'requests left: ' . count($left);
};
