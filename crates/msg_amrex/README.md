# msg_amrex

`msg_amrex` is a library-only importer for converting AMReX plotfiles into the
source-independent `multires::MsgDataset` model.

```rust
use msg_amrex::{AmrexImportOptions, PlotFile, import_amrex};

let plotfile = PlotFile::open("plt00010")?;
let (dataset, report) = import_amrex(&plotfile, &AmrexImportOptions::default())?;
println!("imported {} scalar samples", report.sample_count);
# Ok::<(), anyhow::Error>(())
```

Individual `AmrexField` requests may include `MsgNormalization` bounds. Values
are mapped and clamped into `[0, 1]` in `f64` before conversion to the requested
logical scalar type, and the original physical bounds remain attached to the
resulting MSG field.

Use the sibling `msg_tools` package for TOML-driven AMReX or VTI conversion
and MSG archive encoding.
