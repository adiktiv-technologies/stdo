# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.3](https://github.com/adiktiv-technologies/stdo/compare/v0.5.2...v0.5.3) - 2026-11-11

### Added

- *(studio,examples)* add YouTube capsule generator workflow and inline MP4 video player ([#28](https://github.com/adiktiv-technologies/stdo/pull/28))

### Fixed

- *(assets,readme)* compress pronunciation MP4 under 300KB and remove README spoilers ([#30](https://github.com/adiktiv-technologies/stdo/pull/30))

### Other

- *(readme)* use inline-playable CDN URL for pronunciation speaker link ([#31](https://github.com/adiktiv-technologies/stdo/pull/31))
- *(repo)* ignore local agent config files ([#27](https://github.com/adiktiv-technologies/stdo/pull/27))
- *(readme)* align inline WAV example with atomic sink, remove version lag, and test concurrent writers ([#26](https://github.com/adiktiv-technologies/stdo/pull/26))

## [0.5.2](https://github.com/adiktiv-technologies/stdo/compare/v0.5.1...v0.5.2) - 2026-11-10

### Fixed

- *(examples,benchmarks)* use atomic temp-file rename in sinks, record OS RSS, and sync CHANGELOG ([#24](https://github.com/adiktiv-technologies/stdo/pull/24))

### Fixed

- Stage the speech and HTTP examples' output beside the destination and rename only after a successful write.

### Changed

- Record operating-system peak RSS separately from monitored buffers in the qualification benchmark.
- Document the live Apple Silicon Ollama qualification results.

## [0.5.1](https://github.com/adiktiv-technologies/stdo/compare/v0.5.0...v0.5.1) - 2026-11-10

### Added

- Gemma 4 and Phonon speech, cached local PR triage, and a retrying HTTP-to-JSONL workflow. Studio also gained live run timers, per-step output cards, binary summaries, ANSI cleanup, and inline audio playback with downloads. [#22](https://github.com/adiktiv-technologies/stdo/pull/22)

### Changed

- Added the Parallel Bash benchmark baseline and documented how pipe modes, capture, caching, and retries compose. [#21](https://github.com/adiktiv-technologies/stdo/pull/21)

## [0.5.0](https://github.com/adiktiv-technologies/stdo/compare/v0.4.0...v0.5.0) - 2026-11-10

### Added

- Bounded-memory pass-through for uncaptured stdout, atomic pipe gating, Unix process-group cleanup, executable content hashing for cache keys, and a reproducible qualification suite.
- Studio's Visual Command Builder with editable script blocks, raw JSON sync, and shell positional bindings. [#19](https://github.com/adiktiv-technologies/stdo/pull/19)

## [0.4.0](https://github.com/adiktiv-technologies/stdo/compare/v0.3.0...v0.4.0) - 2026-11-10

### Added

- Concurrent producer-consumer streaming across eligible `pipe_from` edges. [#15](https://github.com/adiktiv-technologies/stdo/pull/15)
- Step deadlines, retry with backoff, and cache keys that include declared file and environment dependencies. [#18](https://github.com/adiktiv-technologies/stdo/pull/18)

### Fixed

- Updated the README's SQLite example to pass audit data through stdin and extract values with fixed SQL. [#15](https://github.com/adiktiv-technologies/stdo/pull/15)

## [0.3.0](https://github.com/adiktiv-technologies/stdo/compare/v0.2.0...v0.3.0) - 2026-11-10

### Added

- Pipeline comparison benchmarks, time-to-first-byte metrics, and self-contained JSON and HTML execution reports. [#12](https://github.com/adiktiv-technologies/stdo/pull/12)
- Live Studio execution streaming, external workflow reload, and JSONL validation and routing. [#14](https://github.com/adiktiv-technologies/stdo/pull/14)

### Changed

- Streamlined installation and added a Studio preview to the README. [#11](https://github.com/adiktiv-technologies/stdo/pull/11)

## [0.2.0](https://github.com/adiktiv-technologies/stdo/compare/v0.1.0...v0.2.0) - 2026-11-10

### Added

- Execution telemetry, captured stderr on step failures, and gallery workflows that resolve the repository from either supported working directory. [#7](https://github.com/adiktiv-technologies/stdo/pull/7)
- SHA-256 content-addressable step caching. [#9](https://github.com/adiktiv-technologies/stdo/pull/9)
- Embedded, zero-CDN workflow editing and execution through `stdo serve`. [#10](https://github.com/adiktiv-technologies/stdo/pull/10)

### Fixed

- Switched the ARM64 Linux musl binary build to a native ARM64 runner. [#6](https://github.com/adiktiv-technologies/stdo/pull/6)

## [0.1.0](https://github.com/adiktiv-technologies/stdo/compare/v0.0.1...v0.1.0) - 2026-11-10

### Added

- Workflow schema, topological scheduling, and the `stdo doctor` validator. [#3](https://github.com/adiktiv-technologies/stdo/pull/3)
- DAG execution with monitored pipes, JSON parsing, conditional routing, safety limits, and a PID ledger. [#4](https://github.com/adiktiv-technologies/stdo/pull/4)
- Runnable POSIX workflow gallery and end-to-end tests. [#5](https://github.com/adiktiv-technologies/stdo/pull/5)

### Changed

- Established repository governance, release automation, and formatting and lint checks. [#1](https://github.com/adiktiv-technologies/stdo/pull/1)
