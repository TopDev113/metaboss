[![Crate](https://img.shields.io/crates/v/metaboss)](https://crates.io/crates/metaboss)
[![Downloads](https://img.shields.io/crates/d/metaboss)](https://crates.io/crates/metaboss)


# Metaboss

![metaboss logo](mb_logo.gif?raw=true)

## Overview

The Solana Metaplex NFT 'Swiss Army Knife' tool.

Features:

-   Decode the metadata of a token mint account

-   Mint new NFTs from a JSON file or URIs

-   Set `primary_sale_happened` bool on an NFT's metadata

-   Set `update_authority` address on an NFT's metadata

-   Verify a creator by signing the metadata accounts for all tokens in a list, for a given candy machine id or a single mint account

-   Get a snapshot of current NFT holders for a given candy machine ID or update authority

... and more! See the [docs](https://metaboss.rs) for full features and usage instructions.


Suggestions and PRs welcome!

**Note: This is experimental software for a young ecosystem. Use at your own risk. The author is not responsible for misuse of the software or for the user failing to test specific commands on devnet before using on production NFTs.**


## Alternate Tools

Some alternate tools that do similar things:

* [Banana Tools](https://tools.0xbanana.com/)
* [Crypto Straps Tools](https://cryptostraps.tools/nft-mints)
* [SOL Tools](https://sol-tools.tonyboyle.io/nft-tools/create-nft)

## Installation

### Install Binary
Copy the following to a terminal:

If you get errors you may need dependencies:

Ubuntu:

```bash
sudo apt install libssl-dev libudev-dev pkg-config
```

MacOS may need openssl:

```bash
brew install openssl@3
```



### Install From crates.io

```bash
cargo install metaboss
```

### Install From Source

Requires Rust 1.58 or later.

Install [Rust](https://www.rust-lang.org/tools/install).

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Clone the source:


Change directory and check out the `main` branch:

```bash
cd metaboss
git checkout main
```

Install or build with Rust:

```bash
cargo install --path ./
```

or

```bash
cargo build --release
```



