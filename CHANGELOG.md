# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/adiktiv-technologies/stdo/compare/v0.2.0...v0.3.0) - 2026-11-10

### Added

- *(streaming)* add live Studio SSE execution stream, file reload, and JSONL stream routing ([#14](https://github.com/adiktiv-technologies/stdo/pull/14))
- *(bench)* add A/B pipeline benchmark harness, TTFB metrics, and offline audit reports ([#12](https://github.com/adiktiv-technologies/stdo/pull/12))

### Other

- *(readme)* showcase embedded Studio SVG preview and streamline installation ([#11](https://github.com/adiktiv-technologies/stdo/pull/11))

## [0.2.0](https://github.com/adiktiv-technologies/stdo/compare/v0.1.0...v0.2.0) - 2026-11-10

### Added

- *(studio)* add zero-CDN embedded WYSIWYG workflow studio (stdo serve) ([#10](https://github.com/adiktiv-technologies/stdo/pull/10))
- *(cache)* add SHA-256 content-addressable step cache ([#9](https://github.com/adiktiv-technologies/stdo/pull/9))
- *(telemetry)* add --telemetry execution report and improve step error UX ([#7](https://github.com/adiktiv-technologies/stdo/pull/7))

## [0.1.0](https://github.com/adiktiv-technologies/stdo/compare/v0.0.1...v0.1.0) - 2026-11-10

### Added

- *(examples)* [**breaking**] complete Phase 1 DAG engine milestone with runnable workflow gallery ([#5](https://github.com/adiktiv-technologies/stdo/pull/5))
- *(engine)* execute DAGs with monitored pipes and PID reporting ([#4](https://github.com/adiktiv-technologies/stdo/pull/4))
- *(dag)* scaffold the Phase 1 workflow engine ([#3](https://github.com/adiktiv-technologies/stdo/pull/3))

### Fixed

- *(ci)* format code with rustfmt and resolve clippy lints

### Other

- *(repo)* formalize governance and release automation ([#1](https://github.com/adiktiv-technologies/stdo/pull/1))
- add CI and release workflows; create AGENTS.md for architectural guidelines
- update README.md for improved clarity and structure; add .gitignore entries
