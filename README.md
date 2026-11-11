# multires

Rust libraries and command-line tools for multiresolution sparse grids (MSG).

The workspace contains:

- `multires`: the source-independent MSG data model, archive format, and algorithms;
- `msg_amrex`: an AMReX plotfile importer;
- `msg_vti`: a VTK XML ImageData (`.vti`) importer;
- `msg_tools`: the `msg-convert`, `msg-probe`, `msg-transcode`, and
  `msg-isosurface` command-line tools.

Build and test the complete workspace with:

```sh
cargo test --workspace --all-targets --all-features
```

Convert source data or inspect an archive with:

```sh
cargo run -p msg_tools --bin msg-convert -- input.vti output.msg
cargo run -p msg_tools --bin msg-probe -- output.msg
cargo run -p msg_tools --bin msg-transcode -- input.msg output.msg
cargo run -p msg_tools --bin msg-isosurface -- input.msg density 0.5 surface.obj
```

