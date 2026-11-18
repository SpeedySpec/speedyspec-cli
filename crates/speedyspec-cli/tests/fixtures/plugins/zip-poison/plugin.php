<?php

declare(strict_types=1);

// A real engine fatal while loading: the plugin is skipped and PHP is left degraded.
eval('function zip_poison_twice() {} function zip_poison_twice() {}');
