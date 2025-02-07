build-eips
==========

Build system for linting and rendering Ethereum Improvement Proposals ([EIPs] /
[ERCs]).

## Prerequisites

`build-eips` requires a few runtime dependencies, available from wherever you
get your software:

- git
- libgit2
- openssl
- [zola](https://github.com/getzola/zola/tree/next)[^1]

[^1]: Requires at least commit [`ead17d0a3`] for full functionality.

[`ead17d0a3`]: https://github.com/getzola/zola/commit/ead17d0a3a20bfb67043a076c061b35ae6b6ddea

## Installation

### Pre-compiled Binaries

Pre-compiled binaries for Ubuntu, Windows, and macOS are available from
[GitHub Releases].

[GitHub Releases]: https://github.com/ethereum/build-eips/releases

### From Source

If you're feeling particularly adventurous, you can install the latest version
of `build-eips` like so:

```bash
cargo install --git https://github.com/ethereum/build-eips.git
```

[EIPs]: https://github.com/ethereum/EIPs/
[ERCs]: https://github.com/ethereum/ERCs/


## Usage

1. Clone either [`ethereum/EIPs`] or [`ethereum/ERCs`], and change directory
   into it.
1. Modify whatever proposal you'd like.
1. Commit your changes.
1. Build the project. You can use:
    - `build-eips check` to quickly check for problems like missing sections,
      broken internal links, etc.
    - `build-eips build` to create an on-disk bundle of HTML, ready to be
      deployed.
    - `build-eips serve` to launch a web server to preview changes locally.
      **NB: live reload is not yet implemented.**

[`ethereum/EIPs`]: https://github.com/ethereum/EIPs/
[`ethereum/ERCs`]: https://github.com/ethereum/ERCs/
