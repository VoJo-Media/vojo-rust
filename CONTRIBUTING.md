# Contributing

`src/generated/` is generated from the API files in `shared/`. Do not edit it by hand: change `tools/generate.py` and regenerate. The generator needs Python 3.10 or newer, with no packages, and `rustfmt`.

```bash
python3 tools/generate.py --check
cargo clippy --all-features --all-targets
cargo test --all-features
```

The tests include every case in `shared/webhook-vectors.json` and a call to every operation against a stub. The examples in `examples/` and in the README compile as part of `cargo test`.

`shared/CONVENTIONS.md` holds the rules every Vojo library follows.
