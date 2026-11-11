use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use glam::{DVec3, IVec3, UVec3};
use multires_rs::{
    MSG_FORMAT_VERSION, MappedMsgArchive, MsgArchiveCodecStats, MsgArchiveFieldLevelStorage,
    MsgArchiveLevelInfo, MsgChunkCodec, MsgGeometry, MsgNormalization, MsgScalarType,
};

#[derive(Debug, Parser)]
#[command(about = "Inspect MSG metadata, values, codecs, and storage")]
struct Args {
    /// MSG archive to inspect.
    archive: PathBuf,
    /// Validate metadata without decoding scalar chunks for value statistics.
    #[arg(long)]
    metadata_only: bool,
}

#[derive(Debug, Clone, Copy)]
struct CodecTotal {
    codec: MsgChunkCodec,
    codec_id: u32,
    codec_version: u16,
    chunk_count: usize,
    metadata_bytes: u128,
    payload_bytes: u128,
    max_abs_error: f64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let file_size = u128::from(
        fs::metadata(&args.archive)
            .with_context(|| format!("reading metadata for {}", args.archive.display()))?
            .len(),
    );
    let mapped = MappedMsgArchive::open(&args.archive)?;
    let archive = mapped.view()?;
    let levels = archive.levels().collect::<Vec<_>>();
    let fields = archive.fields().collect::<Vec<_>>();
    let geometry = archive.geometry();
    let dense_bytes = dense_scalar_payload_size(&levels, &fields)?;

    println!("archive: {}", args.archive.display());
    println!("format: MSG v{MSG_FORMAT_VERSION}");
    println!("size: {}", format_bytes(file_size));
    println!("simulation time: {:.17}", archive.simulation_time());
    println!("levels: {}", format_count(levels.len() as u128));
    println!("fields: {}", format_count(fields.len() as u128));
    println!(
        "dense scalar payload estimate: {}",
        format_bytes(dense_bytes)
    );
    if file_size != 0 && dense_bytes != 0 {
        println!(
            "archive / dense payload: {:.4}% (dense/archive: {:.3}x)",
            file_size as f64 / dense_bytes as f64 * 100.0,
            dense_bytes as f64 / file_size as f64
        );
    }
    println!("sample offset: {}", format_dvec3(geometry.sample_offset()));
    println!("grid-to-world columns:");
    for column in geometry.grid_to_world().to_cols_array_2d() {
        println!(
            "  [{:>14.7e}, {:>14.7e}, {:>14.7e}, {:>14.7e}]",
            column[0], column[1], column[2], column[3]
        );
    }

    println!("levels:");
    for (index, level) in levels.iter().enumerate() {
        let (world_min, world_max) = level_world_bounds(geometry, *level);
        println!(
            "  [{index}] origin={} points={} scale={} refinement={}",
            format_i32vec3(level.index_origin),
            format_u32vec3(level.point_extent),
            format_f64vec3(level.scale),
            level
                .refinement_ratio_from_coarse
                .map(format_u32vec3)
                .unwrap_or_else(|| "base".to_owned())
        );
        println!(
            "      world sample bounds: {} to {}",
            format_dvec3(world_min),
            format_dvec3(world_max)
        );
    }

    let mut accounted_bytes = 0_u128;
    println!("fields:");
    for (field_index, info) in fields.iter().enumerate() {
        let field = archive
            .field(info.name)
            .expect("field metadata and field lookup disagree");
        let normalization = info
            .normalization
            .map(format_normalization)
            .unwrap_or_else(|| "none".to_owned());
        let error = info
            .error_tolerance
            .map(|error| {
                format!(
                    "absolute={:.7e}, relative={:.7}%",
                    error.absolute, error.relative_percent
                )
            })
            .unwrap_or_else(|| "lossless".to_owned());
        println!(
            "  [{field_index}] {:?}: {:?}, normalization={normalization}, policy={error}",
            info.name, info.scalar_type
        );

        let mut field_storage = 0_u128;
        let mut field_points = 0_u128;
        let mut field_min = f64::INFINITY;
        let mut field_max = f64::NEG_INFINITY;
        let mut codecs = Vec::<CodecTotal>::new();
        for level_index in 0..field.level_count() {
            let level = field.level(level_index)?;
            let storage = level.storage();
            let stored = u128::from(storage.total_bytes());
            field_storage = field_storage
                .checked_add(stored)
                .context("field storage size overflow")?;
            if args.metadata_only {
                println!(
                    "      level {level_index}: chunks={}, active-range-chunks={}, stored={}",
                    format_count(level.chunk_keys().len() as u128),
                    format_count(level.value_ranges().len() as u128),
                    format_bytes(stored)
                );
                print_storage(storage);
                continue;
            }

            let stats = level.statistics()?;
            field_points = field_points
                .checked_add(stats.present_point_count as u128)
                .context("field point count overflow")?;
            if let Some(min) = stats.min {
                field_min = field_min.min(min);
            }
            if let Some(max) = stats.max {
                field_max = field_max.max(max);
            }
            for codec in stats.codecs {
                accumulate_codec(&mut codecs, codec)?;
            }
            println!(
                "      level {level_index}: chunks={}, samples={}, range={}, stored={}",
                format_count(stats.chunk_count as u128),
                format_count(stats.present_point_count as u128),
                format_range(stats.min, stats.max),
                format_bytes(stored)
            );
            print_storage(stats.storage);
        }
        accounted_bytes = accounted_bytes
            .checked_add(field_storage)
            .context("accounted archive size overflow")?;
        let field_dense = dense_field_size(&levels, info.scalar_type)?;
        if !args.metadata_only {
            let range = (field_points != 0).then_some((field_min, field_max));
            println!(
                "      total: samples={}, range={}, stored={}, dense={}, stored/dense={:.4}%",
                format_count(field_points),
                format_optional_range(range),
                format_bytes(field_storage),
                format_bytes(field_dense),
                percentage(field_storage, field_dense)
            );
            if let (Some(normalization), Some((min, max))) = (info.normalization, range) {
                println!(
                    "      reconstructed physical range: {:.7e} to {:.7e}",
                    denormalize(min, normalization),
                    denormalize(max, normalization)
                );
            }
            println!("      codecs:");
            for codec in codecs {
                let physical_error = codec.max_abs_error
                    * info
                        .normalization
                        .map_or(1.0, |normalization| normalization.max - normalization.min);
                let logical_suffix = info
                    .normalization
                    .map(|_| format!(", logical-error={:.7e}", codec.max_abs_error))
                    .unwrap_or_default();
                println!(
                    "        {} id={} v{}: chunks={}, metadata={}, payload={}, max-abs-error={physical_error:.7e}{logical_suffix}",
                    codec.codec.name(),
                    codec.codec_id,
                    codec.codec_version,
                    format_count(codec.chunk_count as u128),
                    format_bytes(codec.metadata_bytes),
                    format_bytes(codec.payload_bytes),
                );
            }
        }
    }

    println!("storage accounting:");
    println!("  field blocks: {}", format_bytes(accounted_bytes));
    println!(
        "  headers, alignment, and other overhead: {}",
        format_bytes(file_size.saturating_sub(accounted_bytes))
    );
    if args.metadata_only {
        println!("value and codec statistics: skipped (--metadata-only)");
    }
    Ok(())
}

fn accumulate_codec(totals: &mut Vec<CodecTotal>, stats: MsgArchiveCodecStats) -> Result<()> {
    let index = totals
        .iter()
        .position(|total| {
            total.codec_id == stats.codec_id && total.codec_version == stats.codec_version
        })
        .unwrap_or_else(|| {
            totals.push(CodecTotal {
                codec: stats.codec,
                codec_id: stats.codec_id,
                codec_version: stats.codec_version,
                chunk_count: 0,
                metadata_bytes: 0,
                payload_bytes: 0,
                max_abs_error: 0.0,
            });
            totals.len() - 1
        });
    let total = &mut totals[index];
    total.chunk_count = total
        .chunk_count
        .checked_add(stats.chunk_count)
        .context("codec chunk count overflow")?;
    total.metadata_bytes = total
        .metadata_bytes
        .checked_add(u128::from(stats.metadata_bytes))
        .context("codec metadata size overflow")?;
    total.payload_bytes = total
        .payload_bytes
        .checked_add(u128::from(stats.payload_bytes))
        .context("codec payload size overflow")?;
    total.max_abs_error = total.max_abs_error.max(stats.max_abs_error);
    Ok(())
}

fn print_storage(storage: MsgArchiveFieldLevelStorage) {
    println!(
        "        blocks: scalar-directory={}, scalar-masks={}, scalar-payload={}, acceleration-directory={}, acceleration-masks={}",
        format_bytes(u128::from(storage.scalar_directory_bytes)),
        format_bytes(u128::from(storage.scalar_mask_bytes)),
        format_bytes(u128::from(storage.scalar_payload_bytes)),
        format_bytes(u128::from(storage.acceleration_directory_bytes)),
        format_bytes(u128::from(storage.acceleration_mask_bytes)),
    );
}

fn dense_scalar_payload_size(
    levels: &[MsgArchiveLevelInfo],
    fields: &[multires_rs::MsgArchiveFieldInfo<'_>],
) -> Result<u128> {
    fields.iter().try_fold(0_u128, |total, field| {
        total
            .checked_add(dense_field_size(levels, field.scalar_type)?)
            .context("dense scalar payload size overflow")
    })
}

fn dense_field_size(levels: &[MsgArchiveLevelInfo], scalar_type: MsgScalarType) -> Result<u128> {
    levels.iter().try_fold(0_u128, |total, level| {
        let points = level
            .point_extent
            .iter()
            .try_fold(1_u128, |count, &extent| {
                count
                    .checked_mul(u128::from(extent))
                    .context("dense point count overflow")
            })?;
        total
            .checked_add(
                points
                    .checked_mul(scalar_width(scalar_type))
                    .context("dense field size overflow")?,
            )
            .context("dense field size overflow")
    })
}

fn scalar_width(scalar_type: MsgScalarType) -> u128 {
    match scalar_type {
        MsgScalarType::F32 => 4,
        MsgScalarType::F64 => 8,
    }
}

fn level_world_bounds(geometry: MsgGeometry, level: MsgArchiveLevelInfo) -> (DVec3, DVec3) {
    let origin = IVec3::from_array(level.index_origin);
    let scale = DVec3::from_array(level.scale);
    let extent = UVec3::from_array(level.point_extent) - UVec3::ONE;
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    for z in [0, extent.z] {
        for y in [0, extent.y] {
            for x in [0, extent.x] {
                let point = geometry.point_to_world(origin, scale, UVec3::new(x, y, z));
                min = min.min(point);
                max = max.max(point);
            }
        }
    }
    (min, max)
}

fn denormalize(value: f64, normalization: MsgNormalization) -> f64 {
    normalization.min + value * (normalization.max - normalization.min)
}

fn format_normalization(normalization: MsgNormalization) -> String {
    format!("[{:.7e}, {:.7e}]", normalization.min, normalization.max)
}

fn format_range(min: Option<f64>, max: Option<f64>) -> String {
    format_optional_range(min.zip(max))
}

fn format_optional_range(range: Option<(f64, f64)>) -> String {
    range
        .map(|(min, max)| format!("{min:.7e} to {max:.7e}"))
        .unwrap_or_else(|| "n/a".to_owned())
}

fn percentage(part: u128, whole: u128) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64 * 100.0
    }
}

fn format_bytes(bytes: u128) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} B", format_count(bytes))
    } else {
        format!("{value:.2} {} ({} bytes)", UNITS[unit], format_count(bytes))
    }
}

fn format_count(value: u128) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index != 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(character);
    }
    output
}

fn format_i32vec3(value: [i32; 3]) -> String {
    format!("({}, {}, {})", value[0], value[1], value[2])
}

fn format_u32vec3(value: [u32; 3]) -> String {
    format!("({}, {}, {})", value[0], value[1], value[2])
}

fn format_f64vec3(value: [f64; 3]) -> String {
    format!("({:.7e}, {:.7e}, {:.7e})", value[0], value[1], value[2])
}

fn format_dvec3(value: DVec3) -> String {
    format!("({:.7e}, {:.7e}, {:.7e})", value.x, value.y, value.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_and_count_formatting_is_human_readable() {
        assert_eq!(format_count(123456789), "123,456,789");
        assert_eq!(format_bytes(1000), "1,000 B");
        assert_eq!(format_bytes(1536), "1.50 KiB (1,536 bytes)");
    }

    #[test]
    fn normalization_reconstructs_physical_values() {
        let normalization = MsgNormalization {
            min: 100.0,
            max: 300.0,
        };
        assert_eq!(denormalize(0.25, normalization), 150.0);
    }

    #[test]
    fn percentage_handles_an_empty_dense_reference() {
        assert_eq!(percentage(0, 0), 0.0);
        assert_eq!(percentage(1, 4), 25.0);
    }
}
