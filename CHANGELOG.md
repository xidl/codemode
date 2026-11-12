# Changelog

## [0.7.0](https://github.com/xidl/codemode/compare/v0.6.0...v0.7.0) (2026-11-12)


### ⚠ BREAKING CHANGES

* use single object arguments with snake_case names
* use operation @rename for wire tool names

### Features

* generate output types from MCP output schemas ([7dff7ca](https://github.com/xidl/codemode/commit/7dff7cab184039b9bc0c9ce03f53acd94fd1dc51)), closes [#17](https://github.com/xidl/codemode/issues/17)
* persist run logs and metrics for execute ([ac9371d](https://github.com/xidl/codemode/commit/ac9371d0f2b4732419091ba40a49814579b67c09)), closes [#22](https://github.com/xidl/codemode/issues/22)
* support file-based execute sources ([3c482f2](https://github.com/xidl/codemode/commit/3c482f25397a3c7983acbd0baeaff1a78f6d342b)), closes [#21](https://github.com/xidl/codemode/issues/21)
* unwrap JSON text results and return structuredContent ([1bcac21](https://github.com/xidl/codemode/commit/1bcac21a76fd36258caab8dd4da8ddb5f3c408b9)), closes [#20](https://github.com/xidl/codemode/issues/20)
* use single object arguments with snake_case names ([13ac511](https://github.com/xidl/codemode/commit/13ac511fc41e79ecf06a118f7ccf497616e79869)), closes [#19](https://github.com/xidl/codemode/issues/19)


### Code Refactoring

* use operation [@rename](https://github.com/rename) for wire tool names ([50db46b](https://github.com/xidl/codemode/commit/50db46b53887bc25d8ed50b691bb97bf0f08ba0b)), closes [#18](https://github.com/xidl/codemode/issues/18)

## [0.6.0](https://github.com/xidl/codemode/compare/v0.5.0...v0.6.0) (2026-11-12)


### Features

* replace mcp bindings with tools ([57c7bd4](https://github.com/xidl/codemode/commit/57c7bd48f31eadbe065471b94e1dd6535e3e8509))

## [0.5.0](https://github.com/xidl/codemode/compare/v0.4.1...v0.5.0) (2026-11-12)


### ⚠ BREAKING CHANGES

* normalize tool results into plain JavaScript values
* use null placeholders for optional positional arguments
* use .cache/codemode runs for temporary docs

### Features

* add searchable MCP tool catalog ([becc416](https://github.com/xidl/codemode/commit/becc41600743298518115867d4811648e46d2f8c))
* expose connected server catalog for discovery ([94164c0](https://github.com/xidl/codemode/commit/94164c06ccddceccc439a44834aacb37b43e77b9))
* normalize tool results into plain JavaScript values ([b74fb50](https://github.com/xidl/codemode/commit/b74fb501121cdfdada5cdf1d85b1d73e4e58d44e))
* support await syntax with pure JavaScript promises ([9091b22](https://github.com/xidl/codemode/commit/9091b223dc9606d39cecba701c6ee1f7f7cf408c))
* use null placeholders for optional positional arguments ([37a7add](https://github.com/xidl/codemode/commit/37a7addc41a4b4c49431413e7d76cbbd6c503b68))


### Code Refactoring

* use .cache/codemode runs for temporary docs ([7fe3a77](https://github.com/xidl/codemode/commit/7fe3a7786614b187de747f086324590187b0bd85))

## [0.4.1](https://github.com/xidl/codemode/compare/v0.4.0...v0.4.1) (2026-11-12)


### Bug Fixes

* package glue binary in release workflow ([1c4a7cd](https://github.com/xidl/codemode/commit/1c4a7cd4fc094994fbd5bcc5d41cf24d734c0969))

## [0.4.0](https://github.com/xidl/codemode/compare/v0.3.0...v0.4.0) (2026-11-12)


### Features

* replace runtime manifest with quickjs glue ([2a636c3](https://github.com/xidl/codemode/commit/2a636c32af5f88ebd5aadef324db76b940ce441b))

## [0.3.0](https://github.com/xidl/codemode/compare/v0.2.0...v0.3.0) (2026-11-10)


### Features

* inject only upstream MCP instructions ([13f3352](https://github.com/xidl/codemode/commit/13f335228e76db45b956689d16757e234597d31e))

## [0.2.0](https://github.com/xidl/codemode/compare/v0.1.1...v0.2.0) (2026-11-09)


### Features

* use positional tool arguments ([831dcb3](https://github.com/xidl/codemode/commit/831dcb36f67fcc74684245b5c7771785a7bca23a))

## [0.1.1](https://github.com/xidl/codemode/compare/v0.1.0...v0.1.1) (2026-11-09)


### Bug Fixes

* harden runtime and release workflow ([4c5d36d](https://github.com/xidl/codemode/commit/4c5d36dff7380d4062693c3c29db88c75761619d))

## 0.1.0 (2026-11-09)


### Features

* initial xidl codemode gateway ([30d5e07](https://github.com/xidl/codemode/commit/30d5e07c8c83af1c3c2f28ee97f14480f05a8a3d))
