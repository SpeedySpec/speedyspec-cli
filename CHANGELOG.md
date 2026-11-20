# Changelog

## [1.2.1](https://github.com/SpeedySpec/speedyspec-cli/compare/v1.2.0...v1.2.1) (2026-11-20)


### Bug Fixes

* **release:** build the commit release-please tags, not the newer code on main ([#20](https://github.com/SpeedySpec/speedyspec-cli/issues/20)) ([6401dcc](https://github.com/SpeedySpec/speedyspec-cli/commit/6401dcc8330d86b2352eaea4f5d9bb6edaa360ac))

## [1.2.0](https://github.com/SpeedySpec/speedyspec-cli/compare/v1.1.0...v1.2.0) (2026-11-20)


### Features

* maintain Kubernetes installs through a runner pod ([#16](https://github.com/SpeedySpec/speedyspec-cli/issues/16)) ([1d08337](https://github.com/SpeedySpec/speedyspec-cli/commit/1d0833776c0b235a6a84a037e1dbe9e600a498bc))


### Bug Fixes

* **build:** keep static archives' symbols out of the Linux PHP CLI's exports ([#18](https://github.com/SpeedySpec/speedyspec-cli/issues/18)) ([83c563b](https://github.com/SpeedySpec/speedyspec-cli/commit/83c563b3bb37201c90a2398ca7204455d2ad4d3d))


### Build System

* embed contracts 1.4.0 ([#15](https://github.com/SpeedySpec/speedyspec-cli/issues/15)) ([bab4a8c](https://github.com/SpeedySpec/speedyspec-cli/commit/bab4a8c22c18f502a06d131e95a392523afde8f3))

## [1.1.0](https://github.com/SpeedySpec/speedyspec-cli/compare/v1.0.1...v1.1.0) (2026-11-19)


### Features

* app foundation: install records, app runner, and app db commands ([#10](https://github.com/SpeedySpec/speedyspec-cli/issues/10)) ([82d7ad6](https://github.com/SpeedySpec/speedyspec-cli/commit/82d7ad6ff912688366459d939ad4f90ac186b5c9))
* back up and restore an install, and move it to a new URL and prefix in Rust ([#13](https://github.com/SpeedySpec/speedyspec-cli/issues/13)) ([bd9a732](https://github.com/SpeedySpec/speedyspec-cli/commit/bd9a7323a5ee5c426ce123fdd3a422b3c4de44a9))
* finish installs, manage plugin and theme files, and install composer packages in Rust ([#12](https://github.com/SpeedySpec/speedyspec-cli/issues/12)) ([0d6242f](https://github.com/SpeedySpec/speedyspec-cli/commit/0d6242f2c9f67407f86cd0a4b43c7fee5aa47527))
* move plugin management to speedyspec self plugin ([#9](https://github.com/SpeedySpec/speedyspec-cli/issues/9)) ([1f1b5ce](https://github.com/SpeedySpec/speedyspec-cli/commit/1f1b5ce958a9b633b7caac7fe437d8abbee9a45a))
* **php:** remove the global hook functions ([#7](https://github.com/SpeedySpec/speedyspec-cli/issues/7)) ([d0c90fb](https://github.com/SpeedySpec/speedyspec-cli/commit/d0c90fb390a2a435df2a1ac039307761bd9653b4))
* the Rust container and WordPress Hook API, copied from speedyspec-server ([#11](https://github.com/SpeedySpec/speedyspec-cli/issues/11)) ([8cb127e](https://github.com/SpeedySpec/speedyspec-cli/commit/8cb127ec567691401245ef57c647b45a5d4581a5))


### Bug Fixes

* review findings for composer, extensions and backup; embed contracts 1.3.0 ([#14](https://github.com/SpeedySpec/speedyspec-cli/issues/14)) ([e57acdb](https://github.com/SpeedySpec/speedyspec-cli/commit/e57acdb4c7c0d3d82f04c168f241e13e802ed2de))

## [1.0.1](https://github.com/SpeedySpec/speedyspec-cli/compare/v1.0.0...v1.0.1) (2026-11-18)


### Bug Fixes

* release builds on Intel macOS and Linux aarch64 ([#5](https://github.com/SpeedySpec/speedyspec-cli/issues/5)) ([49fe5c2](https://github.com/SpeedySpec/speedyspec-cli/commit/49fe5c2b9b7582290d174e226ceb4be5b00b21e5))

## 1.0.0 (2026-11-18)


### Features

* add hook contract and in-memory backend ([5ca5d5f](https://github.com/SpeedySpec/speedyspec-cli/commit/5ca5d5fad6a021035cbd653c4ebe87499bcae6f8))
* add plugin manifest, discovery and loading ([1d44267](https://github.com/SpeedySpec/speedyspec-cli/commit/1d44267a5bff7a9634197425efc2c5276b247492))
* add Rust mirror of app and command contracts ([0fc26fc](https://github.com/SpeedySpec/speedyspec-cli/commit/0fc26fcbea0238a61f0567a002e230d5189098e3))
* add speedyspec binary with plugin commands and app list ([497682a](https://github.com/SpeedySpec/speedyspec-cli/commit/497682a6e4b5e9e9f7ca5a95cd400ea722fcd834))
* app install with the zip driver, plugin install and remove ([#1](https://github.com/SpeedySpec/speedyspec-cli/issues/1)) ([86531d4](https://github.com/SpeedySpec/speedyspec-cli/commit/86531d491a367b5a58e922a07fd106aaf58d0f62))
* expose hook registry to PHP as SpeedySpec\Hooks\Hook ([06256b8](https://github.com/SpeedySpec/speedyspec-cli/commit/06256b8957255a1c01927f8ff942c486ed589a84))
* Kubernetes driver and app uninstall ([#2](https://github.com/SpeedySpec/speedyspec-cli/issues/2)) ([9b6c0bf](https://github.com/SpeedySpec/speedyspec-cli/commit/9b6c0bffc2af83e0a3e8c0ff3008ec23d7576069))
* self update, release builds and pinned contracts ([#3](https://github.com/SpeedySpec/speedyspec-cli/issues/3)) ([c6f4ef7](https://github.com/SpeedySpec/speedyspec-cli/commit/c6f4ef7bbc91c9fecc9f4db8154d5ceb2f23523c))
* support fibers inside hook callbacks ([5bd01f2](https://github.com/SpeedySpec/speedyspec-cli/commit/5bd01f2a8edb4f37cbee1dfd95febca629ea1a72))
* widen hook priority to i64 ([6e7a88e](https://github.com/SpeedySpec/speedyspec-cli/commit/6e7a88e637cef9df46101543fe4fdf782672bed0))


### Bug Fixes

* accept numeric strings for hook priority and accepted args ([80b758d](https://github.com/SpeedySpec/speedyspec-cli/commit/80b758daaa1d0e64a0781f719f9f82cb62b5fb1b))
* attribute contract errors to the exact plugin and keep good apps ([35e65d1](https://github.com/SpeedySpec/speedyspec-cli/commit/35e65d124cd7c404c0ce65a8557d49db17b9f0e7))
* block fiber switches inside hook calls and expose poisoned state ([d8a01db](https://github.com/SpeedySpec/speedyspec-cli/commit/d8a01dba9ec1c7b6ceead691cb344a170298cb59))
* ignore a relative platform data directory for plugins ([8bc05d8](https://github.com/SpeedySpec/speedyspec-cli/commit/8bc05d8fdb0367dafa6ff83bb3993a2743aaa532))
* keep plugin start-up hook output off stdout ([c43c3f5](https://github.com/SpeedySpec/speedyspec-cli/commit/c43c3f5db1b46d76c4a12f0e55202022eabb1369))
* never fall back to a working-directory plugin path ([5008163](https://github.com/SpeedySpec/speedyspec-cli/commit/500816391bf213cc40e25ab176355bfed280ac3d))
* never let a stray destructor exception replace a hook callback's result ([2853310](https://github.com/SpeedySpec/speedyspec-cli/commit/28533104d1d3d00f63c1081134f464611c566c37))
* release removed hook callbacks outside the registry borrow ([47d97fb](https://github.com/SpeedySpec/speedyspec-cli/commit/47d97fb0c24ace669c744df2b3c5819bf783c41c))
* report memory exhaustion as a fatal and keep the engine usable ([8e9fc9c](https://github.com/SpeedySpec/speedyspec-cli/commit/8e9fc9ce96ce2c757d2b682a92b95638f34786d5))
* send PHP diagnostics to stderr, never stdout ([7e9b931](https://github.com/SpeedySpec/speedyspec-cli/commit/7e9b931d3c6bddb8d15d117e9bfeb39628f730a8))
* settle stray destructor exceptions, define STD handles, name hooks in errors ([b1da48d](https://github.com/SpeedySpec/speedyspec-cli/commit/b1da48d1d81a1c8d5fa9ae19a48ee3301b4d3ee4))
* stop build-php.sh installing host packages unless opted in ([7992740](https://github.com/SpeedySpec/speedyspec-cli/commit/7992740f63397832b0cb6db1d91e854e21e44da7))
* survive a closed stdout, never wrap handler status to 0, tighten CLI tests ([4157e23](https://github.com/SpeedySpec/speedyspec-cli/commit/4157e23c47f7fcfd746cff8884cfe5b302e007b4))
* validate plugin entries once and report bad plugin directories ([d3cb100](https://github.com/SpeedySpec/speedyspec-cli/commit/d3cb10044e2aa756d33d793d8d1cf4049346b8e4))


### Code Refactoring

* split PHP boundary and SAPI out of runtime.rs ([4d219a8](https://github.com/SpeedySpec/speedyspec-cli/commit/4d219a81e899855cb94429878021683c11befbfb))


### Documentation

* memory exhaustion, the single request, source layout and i64 priorities ([8571771](https://github.com/SpeedySpec/speedyspec-cli/commit/8571771c17c1031cd1570078a94ea528cc59ef5b))


### Build System

* prove static PHP embed on macOS and Linux ([848a2fe](https://github.com/SpeedySpec/speedyspec-cli/commit/848a2fe9a5720b001ae5be6e1935d55af8f83b58))


### Continuous Integration

* authenticate spc's GitHub API calls with the workflow token ([0051e86](https://github.com/SpeedySpec/speedyspec-cli/commit/0051e869b8b3234ec0362ad55ab94936610d1986))
* check out contracts and restrict permissions ([fad69d0](https://github.com/SpeedySpec/speedyspec-cli/commit/fad69d0adc29b138b16d397a833693c54bd28156))
