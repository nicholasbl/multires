use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use multires::MsgScalarType;
use serde::Deserialize;

#[derive(Debug, Parser)]
#[command(
    name = "msg-convert",
    about = "Convert supported scientific data formats into MSG archives",
    after_help = "Formats are inferred from the input and output paths when possible. With --config, positional paths may override both configured paths as a pair."
)]
pub(crate) struct Cli {
    #[arg(long, value_name = "FILE")]
    pub(crate) config: Option<PathBuf>,

    #[arg(long, value_enum)]
    pub(crate) input_format: Option<InputFormat>,

    #[arg(long, value_enum)]
    pub(crate) output_format: Option<OutputFormat>,

    /// Select an input field or array. Repeat to select more than one.
    #[arg(short, long, value_name = "NAME", action = clap::ArgAction::Append, conflicts_with = "config")]
    pub(crate) field: Vec<String>,
    /// Replace invalid AMReX values with zero.
    #[arg(long, conflicts_with = "config")]
    pub(crate) amrex_replace_invalid: bool,
    pub(crate) input: Option<PathBuf>,
    pub(crate) output: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum InputFormat {
    Amrex,
    ImageStack,
    Vti,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum OutputFormat {
    Msg,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) input: InputConfig,
    pub(crate) output: OutputConfig,
    pub(crate) source: SourceConfig,
    pub(crate) encoding: EncodingConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct InputConfig {
    pub(crate) path: Option<PathBuf>,
    pub(crate) format: Option<InputFormat>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct OutputConfig {
    pub(crate) path: Option<PathBuf>,
    pub(crate) format: Option<OutputFormat>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SourceConfig {
    pub(crate) fields: Vec<SourceFieldSelection>,
    pub(crate) amrex: Option<AmrexSourceConfig>,
    #[serde(rename = "image-stack")]
    pub(crate) image_stack: Option<ImageStackSourceConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ImageStackSourceConfig {
    pub(crate) field: String,
    pub(crate) spacing: [f64; 3],
    pub(crate) discard: Vec<u16>,
}

impl Default for ImageStackSourceConfig {
    fn default() -> Self {
        Self {
            field: "labels".to_owned(),
            spacing: [1.0; 3],
            discard: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum SourceFieldSelection {
    Name(String),
    Detailed(SourceFieldConfig),
}

impl SourceFieldSelection {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Name(name) => name,
            Self::Detailed(field) => &field.name,
        }
    }

    pub(crate) fn precision(&self) -> ConfigScalarType {
        match self {
            Self::Name(_) => ConfigScalarType::F32,
            Self::Detailed(field) => field.precision.unwrap_or(ConfigScalarType::F32),
        }
    }

    pub(crate) fn has_precision(&self) -> bool {
        matches!(self, Self::Detailed(field) if field.precision.is_some())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceFieldConfig {
    pub(crate) name: String,
    pub(crate) precision: Option<ConfigScalarType>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AmrexSourceConfig {
    pub(crate) invalid_values: InvalidValueConfig,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum InvalidValueConfig {
    #[default]
    Reject,
    Zero,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EncodingConfig {
    pub(crate) defaults: EncodingDefaultsConfig,
    pub(crate) fields: Vec<EncodingFieldConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EncodingDefaultsConfig {
    pub(crate) codec: Option<String>,
    pub(crate) error: Option<ErrorConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EncodingFieldConfig {
    pub(crate) name: String,
    pub(crate) codec: Option<String>,
    pub(crate) error: Option<ErrorConfig>,
    pub(crate) normalize: Option<NormalizationConfig>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NormalizationConfig {
    pub(crate) min: f64,
    pub(crate) max: f64,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ErrorConfig {
    pub(crate) absolute: Option<f64>,
    pub(crate) relative_percent: Option<f64>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ConfigScalarType {
    F32,
    F64,
}

impl From<ConfigScalarType> for MsgScalarType {
    fn from(value: ConfigScalarType) -> Self {
        match value {
            ConfigScalarType::F32 => Self::F32,
            ConfigScalarType::F64 => Self::F64,
        }
    }
}
