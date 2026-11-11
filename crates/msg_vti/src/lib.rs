//! VTK XML ImageData (`.vti`) import into point-associated MSG datasets.
//!
//! This crate owns VTI parsing only. The MSG model and `.msg` serialization
//! remain in `multires_rs`.

mod parser;

pub use parser::{VtiImportOptions, VtiImportTimings, import_vti};
