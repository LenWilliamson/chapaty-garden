# Chapaty Garden

[![Discord](https://img.shields.io/discord/1495690333911257108.svg?label=Discord&logo=discord&color=7289da&logoColor=white)][discord]
[![CI (Main)](https://github.com/LenWilliamson/chapaty-garden/actions/workflows/ci.yaml/badge.svg?branch=main)](https://github.com/LenWilliamson/chapaty-garden/actions/workflows/ci.yaml)
[![CI (Develop)](https://github.com/LenWilliamson/chapaty-garden/actions/workflows/ci.yaml/badge.svg?branch=develop)](https://github.com/LenWilliamson/chapaty-garden/actions/workflows/ci.yaml)
[![Chapaty](https://img.shields.io/crates/v/chapaty.svg?label=chapaty)][chapaty-crate]

The fastest way to build quantitative trading agents in Rust.

[▶️ **Try Web Demo**][chapaty] | [🔬 **Read Deep Dive**][blog-deep-dive] | [👾 **Join Discord**][discord]

---

**Welcome to Chapaty-Garden!** A library of ready-to-run quantitative trading strategies for the Chapaty backtesting engine. Built from [chapaty-template].

## Quick Start (60 seconds)

Windows users: Please read the [Windows Users](#windows-users) section before proceeding.

```bash
# 1. Clone the garden (we use 'cg' as a shorthand directory name)
git clone --depth 1 https://github.com/LenWilliamson/chapaty-garden.git cg
cd cg

# 2. Check dependencies (Rust + Python)
make doctor

# 3. Compile the project and install visualization dependencies
make setup

# 4. Run the shipped demo agent and generate a tearsheet
make run

# 5. Open the resulting HTML report
open chapaty/reports/demo/tearsheet.html        # macOS
# xdg-open chapaty/reports/demo/tearsheet.html  # Linux
```

## Prerequisites

| Tool                         | Installation                                                                                            |
| ---------------------------- | ------------------------------------------------------------------------------------------------------- |
| **Rust** (`rustup`, `cargo`) | [rust-lang.org/tools/install](https://www.rust-lang.org/tools/install) (Requires 1.98.0+, Edition 2024) |
| **Python** (`3.13.1+`)       | [pyenv](https://github.com/pyenv/pyenv#installation) is recommended.                                    |
| **LLM Environment**          | Claude Code, DeepSeek, Gemini CLI, Aider, Cursor, etc.                                                  |

## Windows Users

The included `Makefile` is designed for Unix-like systems. To run this project on Windows, you have a few options:

1. **WSL (Windows Subsystem for Linux)**: Recommended. Runs the Makefile and paths natively.
2. **Git Bash**: Ships with a `make`-compatible shell and covers most commands.

## Market Data (Free via Hugging Face)

Chapaty uses pre-compiled `.postcard` environments hosted for free on [Hugging Face Datasets][hf-datasets]. Your first `make run` automatically downloads and caches the required data locally.

Need a different dataset or timeframe? Drop a request in the `#data-requests` channel on [Discord][discord].

## Disclaimer

**Trading and investing involve substantial risk. You may lose some or all of your capital.**

Chapaty is an **open-source software project** provided for **research and educational purposes only**. It **does not constitute financial, investment, legal, or trading advice**.

This software is provided **“AS IS”**, without warranties or conditions of any kind, express or implied, as stated in the **Apache License, Version 2.0**. The software may contain bugs, errors, or inaccuracies.

**In no event shall the authors or contributors be liable for any direct or indirect losses, damages, or consequences**, including but not limited to financial losses, arising from the use of this software.

By using Chapaty, you acknowledge that **you are solely responsible for any trading decisions, strategies, or outcomes**.

[chapaty]: https://chapaty.com
[discord]: https://discord.gg/MmMAB6NCuK
[chapaty-crate]: https://crates.io/crates/chapaty
[hf-datasets]: https://huggingface.co/datasets/chapaty/environments
[blog-deep-dive]: https://dev.to/len_chapaty/an-open-source-gym-style-backtesting-framework-for-algorithmic-trading-in-rust-53fg
[chapaty-template]: https://github.com/LenWilliamson/chapaty-template
