# fe2o3-rpnx-core

![release](https://badgen.net/github/release/isene/rpnx-core) ![Rust](https://img.shields.io/badge/language-Rust-f74c00) ![License](https://img.shields.io/badge/license-Unlicense-green)

The RPN / XRPN calculator engine that powers **[RPNx](https://github.com/isene/rpnx)**
(the terminal calculator) and the **[RPNx Android app](https://github.com/isene/nomad/tree/master/apps/rpnx)**.
One engine, two frontends, identical results. Part of the
[Fe₂O₃](https://github.com/isene/fe2o3) suite.

Pure logic, no I/O: an HP-41CX-style stack (X/Y/Z/T + Last X), the full
math / trig / log / stats / base / HMS / polar command set, number formatting
faithful to desktop [XRPN](https://github.com/isene/xrpn), and a FOCAL program
runner (labels, GTO/XEQ/GSB/RTN, ISG/DSE, conditionals, PROMPT). Every entry
point takes a `CalcState` and returns new state, so it drops straight into any
UI.

```rust
use rpnx_core::{new_state, key_digit, key_enter, execute, display};

let s = new_state();
let s = key_digit(s, "3".into());
let s = key_enter(s);
let s = key_digit(s, "4".into());
let s = execute(s, "add".into()).state;   // Y=3, X=4, "+" -> 7
assert_eq!(display(s).x, "7,0000");
```

## Features

- `uniffi` (off by default) — exposes the UniFFI surface the RPNx Android app's
  Kotlin shell binds to. Desktop frontends link the engine directly and leave it
  off.

## License

[Unlicense](https://unlicense.org/) — public domain.
