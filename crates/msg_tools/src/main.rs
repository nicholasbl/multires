pub(crate) mod amrex;
pub(crate) mod image_stack;
pub(crate) mod schema;

use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use clap::Parser;

use anyhow::{Context, Result, bail, ensure};

use msg_amrex::{AmrexField, AmrexImportOptions, InvalidValuePolicy, PlotFile, import_amrex};
use msg_vti::{VtiImportOptions, import_vti};
use multires::{
    MsgChunkCodec, MsgChunkCodecSelection, MsgDataset, MsgErrorTolerance, MsgFieldCodec,
    MsgNormalization, MsgWriteOptions, write_msg,
};

use schema::*;

fn main() -> Result<()> {
    let resolved = Resolved::resolve(Cli::parse())?;
    reject_same_file(&resolved.input, &resolved.output)?;

    let (mut dataset, import_description) = match resolved.input_format {
        InputFormat::Amrex => amrex::import_from_amrex(&resolved)?,
        InputFormat::Vti => import_from_vti(&resolved)?,
    };
    normalize_fields(&mut dataset, &resolved.config)?;
    let codecs = codec_policies(&dataset, &resolved.config)?;
    let file = File::create(&resolved.output)
        .with_context(|| format!("creating MSG archive {}", resolved.output.display()))?;
    let mut writer = BufWriter::new(file);
    let archive_start = Instant::now();
    write_msg(&dataset, &MsgWriteOptions { codecs }, &mut writer)
        .with_context(|| format!("writing MSG archive {}", resolved.output.display()))?;
    let archive_time = archive_start.elapsed();
    let flush_start = Instant::now();
    writer.flush()?;
    let flush_time = flush_start.elapsed();

    eprintln!("{import_description}");
    eprintln!(
        "MSG output: fields {}, levels {}, archive {:.6}s, final flush {:.6}s",
        dataset.fields().len(),
        dataset.levels().len(),
        archive_time.as_secs_f64(),
        flush_time.as_secs_f64()
    );
    Ok(())
}

struct Resolved {
    input: PathBuf,
    output: PathBuf,
    input_format: InputFormat,
    config: Config,
    cli_fields: Vec<String>,
    cli_replace_invalid: bool,
}

impl Resolved {
    fn resolve(cli: Cli) -> Result<Self> {
        let mut config = if let Some(path) = &cli.config {
            let source = fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?;
            toml::from_str(&source).with_context(|| format!("parsing config {}", path.display()))?
        } else {
            Config::default()
        };

        let (input, output) = match (cli.input, cli.output) {
            (Some(input), Some(output)) => (input, output),
            (None, None) => (
                config
                    .input
                    .path
                    .take()
                    .context("missing INPUT path on the command line and in the config")?,
                config
                    .output
                    .path
                    .take()
                    .context("missing OUTPUT path on the command line and in the config")?,
            ),
            _ => bail!("input and output path overrides must be provided together"),
        };

        let input_format = cli
            .input_format
            .or(config.input.format)
            .map(Ok)
            .unwrap_or_else(|| infer_input_format(&input))?;
        let output_format = cli
            .output_format
            .or(config.output.format)
            .map(Ok)
            .unwrap_or_else(|| infer_output_format(&output))?;
        ensure!(
            output_format == OutputFormat::Msg,
            "only MSG output is currently supported"
        );
        validate_config_for_format(&config, input_format)?;

        Ok(Self {
            input,
            output,
            input_format,
            config,
            cli_fields: cli.field,
            cli_replace_invalid: cli.amrex_replace_invalid,
        })
    }
}

fn infer_input_format(path: &Path) -> Result<InputFormat> {
    if extension_is(path, "vti") {
        return Ok(InputFormat::Vti);
    }
    if path.is_dir() && path.join("Header").is_file() {
        return Ok(InputFormat::Amrex);
    }
    bail!(
        "cannot infer the input format from {}; use --input-format or [input].format in TOML",
        path.display()
    )
}

fn infer_output_format(path: &Path) -> Result<OutputFormat> {
    if extension_is(path, "msg") {
        Ok(OutputFormat::Msg)
    } else {
        bail!(
            "cannot infer the output format from {}; use --output-format or [output].format in TOML",
            path.display()
        )
    }
}

fn extension_is(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

fn validate_config_for_format(config: &Config, format: InputFormat) -> Result<()> {
    if format == InputFormat::Vti {
        ensure!(
            config.source.amrex.is_none(),
            "[source.amrex] options cannot be used with VTI input"
        );
        ensure!(
            config
                .source
                .fields
                .iter()
                .all(|field| !field.has_precision()),
            "source field precision is supported only for AMReX input"
        );
    }
    for (index, field) in config.source.fields.iter().enumerate() {
        ensure!(
            !config.source.fields[..index]
                .iter()
                .any(|earlier| earlier.name() == field.name()),
            "source field {:?} is configured more than once",
            field.name()
        );
    }
    match (
        &config.encoding.defaults.codec,
        config.encoding.defaults.error,
    ) {
        (None, None) => {}
        (None, Some(_)) => bail!("[encoding.defaults].error requires a codec"),
        (Some(codec), error) => {
            parse_codec(codec)?;
            resolve_error(error)?;
        }
    }
    for (index, field) in config.encoding.fields.iter().enumerate() {
        ensure!(
            !config.encoding.fields[..index]
                .iter()
                .any(|earlier| earlier.name == field.name),
            "encoding field {:?} is configured more than once",
            field.name
        );
        ensure!(
            field.codec.is_some() || field.normalize.is_some(),
            "encoding field {:?} must configure codec or normalize",
            field.name
        );
        match (&field.codec, field.error) {
            (None, None) => {}
            (None, Some(_)) => bail!(
                "encoding field {:?} has an error tolerance but no codec",
                field.name
            ),
            (Some(codec), error) => {
                parse_codec(codec)?;
                resolve_error(error)?;
            }
        }
        if let Some(normalization) = field.normalize {
            resolve_normalization(normalization)?;
        }
    }
    Ok(())
}

fn import_from_vti(resolved: &Resolved) -> Result<(MsgDataset, String)> {
    let fields = if resolved.cli_fields.is_empty() {
        resolved
            .config
            .source
            .fields
            .iter()
            .map(|field| field.name().to_owned())
            .collect()
    } else {
        resolved.cli_fields.clone()
    };
    let (dataset, timings) = import_vti(&resolved.input, &VtiImportOptions { fields })?;
    Ok((
        dataset,
        format!(
            "VTI import: mmap {:.6}s, UTF-8 preparation {:.6}s, XML parse {:.6}s",
            timings.mmap.as_secs_f64(),
            timings.parse_utf8.as_secs_f64(),
            timings.parse_document.as_secs_f64()
        ),
    ))
}

fn normalize_fields(dataset: &mut MsgDataset, config: &Config) -> Result<()> {
    let configured = config
        .encoding
        .fields
        .iter()
        .filter_map(|field| field.normalize.map(|bounds| (field.name.as_str(), bounds)))
        .collect::<Vec<_>>();
    for (index, &(name, bounds)) in configured.iter().enumerate() {
        ensure!(
            !configured[..index]
                .iter()
                .any(|(earlier, _)| *earlier == name),
            "MSG output field {name:?} is normalized more than once"
        );
        let normalization = resolve_normalization(bounds)?;
        let field = dataset
            .field_mut(name)
            .with_context(|| format!("MSG dataset has no output field {name:?}"))?;
        if let Some(existing) = field.normalization() {
            ensure!(
                existing == normalization,
                "MSG output field {name:?} has conflicting normalization bounds"
            );
            continue;
        }
        for level in field.levels_mut() {
            level
                .values_mut()
                .map_values(|value| normalize_value(value, normalization));
        }
        field.set_normalization(Some(normalization));
    }
    Ok(())
}

fn normalize_value(value: f64, normalization: MsgNormalization) -> f64 {
    ((value - normalization.min) / (normalization.max - normalization.min)).clamp(0.0, 1.0)
}

fn configured_normalization(config: &Config, name: &str) -> Result<Option<MsgNormalization>> {
    let bounds = config
        .encoding
        .fields
        .iter()
        .filter(|field| field.name == name)
        .filter_map(|field| field.normalize)
        .collect::<Vec<_>>();
    ensure!(
        bounds.len() <= 1,
        "MSG output field {name:?} is normalized more than once"
    );
    bounds
        .first()
        .copied()
        .map(resolve_normalization)
        .transpose()
}

fn resolve_normalization(bounds: NormalizationConfig) -> Result<MsgNormalization> {
    let NormalizationConfig { min, max } = bounds;
    ensure!(
        min.is_finite() && max.is_finite() && min < max,
        "normalization bounds must be finite and strictly increasing"
    );
    Ok(MsgNormalization { min, max })
}

fn codec_policies(dataset: &MsgDataset, config: &Config) -> Result<Vec<MsgFieldCodec>> {
    let default = match (
        &config.encoding.defaults.codec,
        config.encoding.defaults.error,
    ) {
        (None, None) => None,
        (None, Some(_)) => bail!("[encoding.defaults].error requires a codec"),
        (Some(codec), error) => Some((parse_codec(codec)?, resolve_error(error)?)),
    };
    let mut policies = dataset
        .fields()
        .iter()
        .filter_map(|field| {
            default.map(|(selection, error_tolerance)| MsgFieldCodec {
                field: field.name().to_owned(),
                selection,
                error_tolerance,
            })
        })
        .collect::<Vec<_>>();

    for (index, field) in config.encoding.fields.iter().enumerate() {
        ensure!(
            !config.encoding.fields[..index]
                .iter()
                .any(|earlier| earlier.name == field.name),
            "MSG output field {:?} is configured more than once",
            field.name
        );
        ensure!(
            field.codec.is_some() || field.normalize.is_some(),
            "MSG output field {:?} must configure codec or normalize",
            field.name
        );
        match (&field.codec, field.error) {
            (None, None) => {}
            (None, Some(_)) => bail!(
                "MSG output field {:?} has an error tolerance but no codec",
                field.name
            ),
            (Some(codec), error) => {
                set_field_policy(dataset, &mut policies, &field.name, codec, error)?;
            }
        }
    }
    Ok(policies)
}

fn set_field_policy(
    dataset: &MsgDataset,
    policies: &mut Vec<MsgFieldCodec>,
    name: &str,
    codec: &str,
    error: Option<ErrorConfig>,
) -> Result<()> {
    ensure!(
        dataset.field(name).is_some(),
        "MSG dataset has no output field {name:?}"
    );
    let policy = MsgFieldCodec {
        field: name.to_owned(),
        selection: parse_codec(codec)?,
        error_tolerance: resolve_error(error)?,
    };
    if let Some(existing) = policies.iter_mut().find(|item| item.field == name) {
        *existing = policy;
    } else {
        policies.push(policy);
    }
    Ok(())
}

fn resolve_error(error: Option<ErrorConfig>) -> Result<Option<MsgErrorTolerance>> {
    error
        .map(|error| {
            ensure!(
                error.absolute.is_some() || error.relative_percent.is_some(),
                "error must specify absolute, relative_percent, or both"
            );
            let absolute = error.absolute.unwrap_or(0.0);
            let relative_percent = error.relative_percent.unwrap_or(0.0);
            ensure!(
                absolute.is_finite() && absolute >= 0.0,
                "absolute error must be finite and non-negative"
            );
            ensure!(
                relative_percent.is_finite() && (0.0..=100.0).contains(&relative_percent),
                "relative_percent must be finite and between 0 and 100"
            );
            Ok(MsgErrorTolerance {
                absolute,
                relative_percent,
            })
        })
        .transpose()
}

fn parse_codec(value: &str) -> Result<MsgChunkCodecSelection> {
    Ok(match value {
        "auto:lossless" => MsgChunkCodecSelection::AutoLossless,
        "auto:smallest" => MsgChunkCodecSelection::AutoSmallest,
        "uniform:f64" => MsgChunkCodecSelection::Forced(MsgChunkCodec::UniformF64),
        "dense:f32" => MsgChunkCodecSelection::Forced(MsgChunkCodec::DenseF32),
        "dense:f64" => MsgChunkCodecSelection::Forced(MsgChunkCodec::DenseF64),
        "quantized:u8" => MsgChunkCodecSelection::Forced(MsgChunkCodec::QuantizedU8),
        "quantized:u16" => MsgChunkCodecSelection::Forced(MsgChunkCodec::QuantizedU16),
        "quantized:u32" => MsgChunkCodecSelection::Forced(MsgChunkCodec::QuantizedU32),
        "affine:f64" => MsgChunkCodecSelection::Forced(MsgChunkCodec::AffineF64),
        "affine-residual:u8" => MsgChunkCodecSelection::Forced(MsgChunkCodec::AffineResidualU8),
        "affine-residual:u16" => MsgChunkCodecSelection::Forced(MsgChunkCodec::AffineResidualU16),
        "affine-residual:u32" => MsgChunkCodecSelection::Forced(MsgChunkCodec::AffineResidualU32),
        _ => bail!("unsupported MSG codec {value:?}; expected family:value"),
    })
}

fn reject_same_file(input: &Path, output: &Path) -> Result<()> {
    if input.is_file() && output.exists() {
        ensure!(
            fs::canonicalize(input)? != fs::canonicalize(output)?,
            "input and output must be different files"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_extensions_case_insensitively() {
        assert_eq!(
            infer_input_format(Path::new("sample.VTI")).unwrap(),
            InputFormat::Vti
        );
        assert_eq!(
            infer_output_format(Path::new("sample.MSG")).unwrap(),
            OutputFormat::Msg
        );
    }

    #[test]
    fn parses_modern_vti_configuration() {
        let config: Config = toml::from_str(
            r#"
[input]
path = "sample.vti"

[output]
path = "sample.msg"

[source]
fields = ["density", "velocity"]

[encoding.defaults]
codec = "auto:lossless"

[[encoding.fields]]
name = "velocity_x"
codec = "quantized:u16"
error = { relative_percent = 0.1 }
"#,
        )
        .unwrap();
        assert_eq!(
            config
                .source
                .fields
                .iter()
                .map(SourceFieldSelection::name)
                .collect::<Vec<_>>(),
            ["density", "velocity"]
        );
        assert_eq!(config.encoding.fields.len(), 1);
    }

    #[test]
    fn parses_amrex_configuration() {
        let config: Config = toml::from_str(
            r#"
[input]
path = "plt00010"
format = "amrex"

[output]
path = "plt00010.msg"

[source]
fields = [{ name = "density", precision = "f64" }]

[source.amrex]
invalid_values = "zero"

[[encoding.fields]]
name = "density"
codec = "auto:smallest"
normalize = { min = 1.0e20, max = 1.000000001e20 }
error = { absolute = 0.0, relative_percent = 0.01 }
"#,
        )
        .unwrap();
        assert_eq!(config.source.fields.len(), 1);
        assert_eq!(
            config.source.amrex.as_ref().unwrap().invalid_values,
            InvalidValueConfig::Zero
        );
        assert_eq!(
            configured_normalization(&config, "density").unwrap(),
            Some(MsgNormalization {
                min: 1.0e20,
                max: 1.000000001e20
            })
        );
    }

    #[test]
    fn rejects_removed_configuration_shapes() {
        assert!(
            toml::from_str::<Config>(
                r#"
input = "sample.vti"
output = "sample.msg"

[vti]
fields = ["density"]
"#,
            )
            .is_err()
        );
        assert!(
            toml::from_str::<Config>(
                r#"
[[variables]]
name = "density"
"#,
            )
            .is_err()
        );
    }

    #[test]
    fn parses_annotated_example_configurations() {
        let examples = [
            (include_str!("../examples/amrex.toml"), InputFormat::Amrex),
            (include_str!("../examples/vti.toml"), InputFormat::Vti),
            (include_str!("../examples/codecs.toml"), InputFormat::Vti),
        ];

        for (source, input_format) in examples {
            let config: Config = toml::from_str(source).unwrap();
            validate_config_for_format(&config, input_format).unwrap();

            if let Some(codec) = &config.encoding.defaults.codec {
                parse_codec(codec).unwrap();
                resolve_error(config.encoding.defaults.error).unwrap();
            }
            for field in &config.encoding.fields {
                if let Some(codec) = &field.codec {
                    parse_codec(codec).unwrap();
                }
                resolve_error(field.error).unwrap();
            }
        }
    }

    #[test]
    fn normalizes_values_and_preserves_original_bounds() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/assets/simple.vti");
        let (mut dataset, _) = import_vti(&path, &VtiImportOptions::default()).unwrap();
        let config: Config = toml::from_str(
            r#"
[[encoding.fields]]
name = "density"
normalize = { min = 0.0, max = 2.0 }
"#,
        )
        .unwrap();

        normalize_fields(&mut dataset, &config).unwrap();
        let normalization = MsgNormalization { min: 0.0, max: 2.0 };
        assert_eq!(normalize_value(-1.0, normalization), 0.0);
        assert_eq!(normalize_value(1.0, normalization), 0.5);
        assert_eq!(normalize_value(3.0, normalization), 1.0);
        assert_eq!(
            dataset.field("density").unwrap().normalization(),
            Some(normalization)
        );
        dataset.validate().unwrap();

        let mut bytes = Vec::new();
        write_msg(&dataset, &MsgWriteOptions::default(), &mut bytes).unwrap();
        assert_eq!(
            multires::view_msg(&bytes)
                .unwrap()
                .field("density")
                .unwrap()
                .normalization(),
            Some(normalization)
        );
    }
}
