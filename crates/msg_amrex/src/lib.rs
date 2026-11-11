//! Import AMReX plotfiles into source-independent MSG datasets.
//!
//! This library owns AMReX plotfile reading and conversion into
//! [`multires_rs::MsgDataset`]. Command-line conversion and MSG archive output
//! policy live in the `msg_convert` crate.

mod import;
mod input;

pub use import::{
    AmrexField, AmrexImportOptions, AmrexImportReport, InvalidValuePolicy, import_amrex,
};
pub use input::{
    BoundingBox, BoxInfo, ByteOrder, CellHeader, ComponentBounds, ComponentView, CoordinateSystem,
    DataReader, Header, IndexDomain, Level, LevelComponents, Levels, Patch, Patches,
    PerLevelRecord, PlotFile, ScalarType, V1StorageMode, Values, Variable, read_header,
    read_hyper_cell_header,
};
