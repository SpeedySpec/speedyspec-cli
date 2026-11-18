<?php

declare(strict_types=1);

/*
 * The runner's bootstrap: loads an app and runs one task inside it, in a child process of the
 * CLI (see runner_child.rs). It runs at PHP's top level on purpose: an app's entry file (for
 * WordPress, wp-load.php and the wp-config.php it reads) sets plain variables that the rest of
 * the app reads as globals, which would be locals inside a function.
 *
 * argv[1] is the request (JSON), argv[2] the file the result is written to. The result is
 * written exactly once, even when the app calls exit() or hits a fatal error, by the shutdown
 * function; every warning, notice and deprecation is recorded with its file and line.
 */

$__speedyspec_runner = new ArrayObject([
    'request' => json_decode((string) file_get_contents($_SERVER['argv'][1]), true, 512, JSON_THROW_ON_ERROR),
    'result_file' => $_SERVER['argv'][2],
    'diagnostics' => [],
    'written' => false,
]);

$__speedyspec_runner['write'] = static function (array $payload) use ($__speedyspec_runner): void
{
    if ($__speedyspec_runner['written'])
    {
        return;
    }
    $__speedyspec_runner['written'] = true;
    $payload['diagnostics'] = $__speedyspec_runner['diagnostics'];
    $json = json_encode($payload, JSON_INVALID_UTF8_SUBSTITUTE | JSON_PARTIAL_OUTPUT_ON_ERROR | JSON_UNESCAPED_SLASHES);
    file_put_contents($__speedyspec_runner['result_file'], $json === false ? '{"ok":false,"error":"the result could not be encoded"}' : $json);
};

$__speedyspec_runner['note'] = static function (string $level, string $message, string $file, int $line) use ($__speedyspec_runner): void
{
    $diagnostics = $__speedyspec_runner['diagnostics'];
    $diagnostics[] = ['level' => $level, 'message' => $message, 'file' => $file, 'line' => $line];
    $__speedyspec_runner['diagnostics'] = $diagnostics;
};

set_error_handler(static function (int $number, string $message, string $file = '', int $line = 0) use ($__speedyspec_runner): bool
{
    $level = match ($number) {
        E_NOTICE, E_USER_NOTICE => 'notice',
        E_DEPRECATED, E_USER_DEPRECATED => 'deprecated',
        E_USER_ERROR, E_RECOVERABLE_ERROR => 'error',
        default => 'warning',
    };
    ($__speedyspec_runner['note'])($level, $message, $file, $line);

    // PHP's own handling goes on as it would without us.
    return false;
});

register_shutdown_function(static function () use ($__speedyspec_runner): void
{
    $last = error_get_last();
    $fatal = E_ERROR | E_PARSE | E_CORE_ERROR | E_COMPILE_ERROR;
    if ($last !== null && ($last['type'] & $fatal) !== 0)
    {
        ($__speedyspec_runner['note'])('error', $last['message'], $last['file'], $last['line']);
        ($__speedyspec_runner['write'])([
            'ok' => false,
            'error' => sprintf('fatal error: %s in %s:%d', $last['message'], $last['file'], $last['line']),
        ]);

        return;
    }
    ($__speedyspec_runner['write'])([
        'ok' => false,
        'error' => 'the task ended before it returned (the app or the task called exit() or die())',
    ]);
});

foreach ($__speedyspec_runner['request']['ini'] as $__speedyspec_name => $__speedyspec_value)
{
    ini_set($__speedyspec_name, $__speedyspec_value);
}
foreach ($__speedyspec_runner['request']['server'] as $__speedyspec_name => $__speedyspec_value)
{
    $_SERVER[$__speedyspec_name] = $__speedyspec_value;
}
unset($__speedyspec_name, $__speedyspec_value);
chdir($__speedyspec_runner['request']['root']);

try
{
    if ($__speedyspec_runner['request']['loads_app'] && $__speedyspec_runner['request']['entry'] !== null)
    {
        require $__speedyspec_runner['request']['root'] . '/' . $__speedyspec_runner['request']['entry'];
    }
    $__speedyspec_task = require $__speedyspec_runner['request']['file'];
    if (!is_callable($__speedyspec_task))
    {
        throw new UnexpectedValueException($__speedyspec_runner['request']['file'] . ' does not return a callable');
    }
    $__speedyspec_result = $__speedyspec_task($__speedyspec_runner['request']['input']);
    ($__speedyspec_runner['write'])(['ok' => true, 'result' => $__speedyspec_result]);
}
catch (Throwable $__speedyspec_thrown)
{
    ($__speedyspec_runner['note'])('exception', $__speedyspec_thrown->getMessage(), $__speedyspec_thrown->getFile(), $__speedyspec_thrown->getLine());
    ($__speedyspec_runner['write'])([
        'ok' => false,
        'error' => sprintf('%s: %s in %s:%d', $__speedyspec_thrown::class, $__speedyspec_thrown->getMessage(), $__speedyspec_thrown->getFile(), $__speedyspec_thrown->getLine()),
    ]);
}
