use crate::*;
use glam::{DMat4, DVec3, IVec3, UVec3};
use multires::{
    MsgField, MsgFieldLevel, MsgLevel, MsgScalarGrid, MsgScalarType, cell_center_to_point_geometry,
    sparse::Aabb3u,
};
use std::collections::HashSet;

pub(crate) fn import_from_image_stack(resolved: &Resolved) -> Result<(MsgDataset, String)> {
    ensure!(
        resolved.cli_fields.is_empty(),
        "--field is not used for image-stack input; configure [source.image-stack].field"
    );
    ensure!(
        !resolved.cli_replace_invalid,
        "--amrex-replace-invalid cannot be used for image-stack input"
    );
    let options = resolved.config.source.image_stack.as_ref();
    let field_name = options.map_or("labels", |options| options.field.as_str());
    let spacing = options.map_or([1.0; 3], |options| options.spacing);
    let discard = options.map_or(&[][..], |options| options.discard.as_slice());
    let paths = collect_candidates(&resolved.input)?;
    ensure!(!paths.is_empty(), "image-stack pattern matched no files");
    let (extent, mut values) = read_stack(&paths)?;
    let discarded = discard_values(&mut values, discard)?;

    let geometry = cell_center_to_point_geometry(DMat4::IDENTITY)
        .context("constructing cell-centered image-stack geometry")?;
    let level = MsgLevel {
        index_origin: IVec3::ZERO,
        point_extent: extent,
        scale: DVec3::from_array(spacing),
        refinement_ratio_from_coarse: None,
    };
    let mut dataset = MsgDataset::new(0.0, geometry, vec![level])?;
    let scalar_type = values.scalar_type();
    dataset.add_field(MsgField::new(
        field_name,
        scalar_type,
        vec![MsgFieldLevel::new(values)],
    ))?;
    Ok((
        dataset,
        format!(
            "image-stack import: {} slices, extent {}x{}x{}, {:?}, {} discarded voxels",
            paths.len(),
            extent.x,
            extent.y,
            extent.z,
            scalar_type,
            discarded
        ),
    ))
}

fn collect_candidates(example: &Path) -> Result<Vec<PathBuf>> {
    let fname = example
        .file_name()
        .and_then(|x| x.to_str())
        .context("converting example file path to string")?;

    use wildmatch::WildMatch;

    let fname_watch = WildMatch::new(fname);

    let containing = example
        .parent()
        .context("obtaining parent directory of example")?;

    let mut paths: Vec<_> = std::fs::read_dir(containing)?
        .filter_map(|dir_item| {
            let Ok(dir_item) = dir_item else {
                return None;
            };

            let file_name = dir_item.file_name();
            let Some(str_name) = file_name.to_str() else {
                return None;
            };

            if !fname_watch.matches(str_name) {
                return None;
            }

            Some(dir_item.path())
        })
        .collect();

    paths.sort_by_key(|path| {
        let key: String = path
            .file_prefix()
            .unwrap_or_default()
            .to_str()
            .unwrap()
            .chars()
            .filter(|x| x.is_ascii_digit())
            .collect();

        let key = key.trim().trim_start_matches(|p: char| p == '0');

        (key.parse::<u64>().unwrap_or_default(), path.clone())
    });

    Ok(paths)
}

fn read_stack(paths: &[PathBuf]) -> Result<(UVec3, MsgScalarGrid)> {
    let first_path = paths.first().context("obtaining first image")?;
    let first =
        image::open(first_path).with_context(|| format!("reading {}", first_path.display()))?;
    let width = first.width();
    let height = first.height();
    let depth = u32::try_from(paths.len()).context("image-stack depth exceeds u32")?;
    let scalar_type = match first.color() {
        image::ColorType::L8 => MsgScalarType::LabelU8,
        image::ColorType::L16 => MsgScalarType::LabelU16,
        color => bail!("image stack must contain single-channel L8 or L16 images, found {color:?}"),
    };
    let extent = UVec3::new(width, height, depth);
    let mut grid = MsgScalarGrid::new(extent, scalar_type);
    for (z, path) in paths.iter().enumerate() {
        let image = image::open(path).with_context(|| format!("reading {}", path.display()))?;
        ensure!(
            image.width() == width && image.height() == height,
            "image {} dimensions differ from the first slice",
            path.display()
        );
        ensure!(
            image.color() == first.color(),
            "image {} scalar type differs from the first slice",
            path.display()
        );
        let z = u32::try_from(z)?;
        let aabb = Aabb3u::new(UVec3::new(0, 0, z), UVec3::new(width, height, z + 1));
        match scalar_type {
            MsgScalarType::LabelU8 => grid
                .write_u8_values(aabb, image.into_luma8().as_raw())
                .map_err(|error| anyhow::anyhow!("writing image slice: {error:?}"))?,
            MsgScalarType::LabelU16 => grid
                .write_u16_values(aabb, image.into_luma16().as_raw())
                .map_err(|error| anyhow::anyhow!("writing image slice: {error:?}"))?,
            _ => unreachable!(),
        }
    }
    Ok((extent, grid))
}

fn discard_values(values: &mut MsgScalarGrid, discard: &[u16]) -> Result<usize> {
    if discard.is_empty() {
        return Ok(0);
    }
    if matches!(values, MsgScalarGrid::LabelU8(_)) {
        ensure!(
            discard.iter().all(|&value| value <= u16::from(u8::MAX)),
            "discard value exceeds the u8 image label range"
        );
    }
    let discard = discard.iter().copied().collect::<HashSet<_>>();
    let mut positions = Vec::new();
    let bounds = values.bounds();
    for z in 0..bounds.z {
        for y in 0..bounds.y {
            for x in 0..bounds.x {
                let position = UVec3::new(x, y, z);
                if values
                    .get(position)
                    .is_some_and(|value| discard.contains(&(value as u16)))
                {
                    positions.push(position);
                }
            }
        }
    }
    for &position in &positions {
        values.clear(position);
    }
    Ok(positions.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma};

    #[test]
    fn reads_sorted_l8_stack_and_discards_explicit_values() -> Result<()> {
        let directory = std::env::temp_dir().join(format!(
            "msg-image-stack-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir(&directory)?;
        for (number, pixels) in [(2, [3, 0]), (1, [1, 2])] {
            let mut image = GrayImage::new(2, 1);
            for (x, value) in pixels.into_iter().enumerate() {
                image.put_pixel(x as u32, 0, Luma([value]));
            }
            image.save(directory.join(format!("slice-{number}.png")))?;
        }

        let paths = collect_candidates(&directory.join("slice-*.png"))?;
        let (extent, mut values) = read_stack(&paths)?;
        assert_eq!(extent, UVec3::new(2, 1, 2));
        assert_eq!(values.scalar_type(), MsgScalarType::LabelU8);
        assert_eq!(values.get(UVec3::new(0, 0, 0)), Some(1.0));
        assert_eq!(values.get(UVec3::new(0, 0, 1)), Some(3.0));
        assert_eq!(discard_values(&mut values, &[0])?, 1);
        assert_eq!(values.get(UVec3::new(1, 0, 1)), None);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
