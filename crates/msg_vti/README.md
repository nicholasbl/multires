# msg_vti

`msg_vti` is a library-only importer for converting VTK XML ImageData (`.vti`)
files into the source-independent `multires_rs::MsgDataset` model.

```rust
use msg_vti::{VtiImportOptions, import_vti};

let (dataset, timings) = import_vti("simulation.vti", &VtiImportOptions::default())?;
println!("imported {} fields", dataset.fields().len());
# Ok::<(), anyhow::Error>(())
```

Use the sibling `msg_convert` command for TOML-driven VTI or AMReX conversion
and MSG archive encoding.
