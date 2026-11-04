# Third-party licences

The direct dependencies of `importin` as locked in `Cargo.lock`, with the licence each declares in its
`Cargo.toml` (from `cargo metadata`). Their own dependencies are not listed.

| crate | version | licence |
|---|---|---|
| `anyhow` | 1.0.104 | MIT OR Apache-2.0 |
| `base64` | 0.23.1 | MIT OR Apache-2.0 |
| `calamine` | 0.38.0 | MIT |
| `chrono` | 0.4.45 | MIT OR Apache-2.0 |
| `dirs` | 7.0.0 | MIT OR Apache-2.0 |
| `glob` | 0.3.4 | MIT OR Apache-2.0 |
| `libc` | 0.2.191 | MIT OR Apache-2.0 |
| `mail-parser` | 1.0.1 | Apache-2.0 OR MIT |
| `pdf-extract` | 0.12.1 | MIT |
| `plist` | 1.10.3 | MIT |
| `quick-xml` | 0.42.0 | MIT |
| `rmcp` | 3.8.0 | Apache-2.0 |
| `rusqlite` | 0.41.1 | MIT |
| `schemars` | 1.2.3 | MIT |
| `serde` | 1.0.229 | MIT OR Apache-2.0 |
| `serde_json` | 1.0.154 | MIT OR Apache-2.0 |
| `tempfile` | 3.27.0 | MIT OR Apache-2.0 |
| `thiserror` | 2.0.21 | MIT OR Apache-2.0 |
| `tokio` | 1.54.1 | MIT |
| `toml` | 1.2.0+spec-1.1.0 | MIT OR Apache-2.0 |
| `zip` | 9.3.0 | MIT |

rusqlite is built with its `bundled` feature, which compiles SQLite (public domain) into the binary.
