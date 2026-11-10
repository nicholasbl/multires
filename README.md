# msg_amrex

`msg_amrex` is a library-only importer for converting AMReX plotfiles into the
source-independent `multires_rs::MsgDataset` model.

```rust
use msg_amrex::{AmrexImportOptions, PlotFile, import_amrex};

let plotfile = PlotFile::open("plt00010")?;
let (dataset, report) = import_amrex(&plotfile, &AmrexImportOptions::default())?;
println!("imported {} scalar samples", report.sample_count);
# Ok::<(), anyhow::Error>(())
```

Use the sibling `msg_convert` command for TOML-driven AMReX or VTI conversion
and MSG archive encoding.
