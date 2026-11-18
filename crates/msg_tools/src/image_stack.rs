use crate::*;

pub(crate) fn import_from_image_stack(resolved: &Resolved) -> Result<(MsgDataset, String)> {
    let example = resolved.input;

    todo!()
}

fn collect_candidates(example: &Path) -> Result<Vec<PathBuf>> {
    let fname = example.file_name().and_then(|x| x.to_str())?;

    use wildmatch::WildMatch;

    let fname_watch = WildMatch::new(fname);

    let containing = example
        .parent()
        .context("obtaining parent directory of example")?;

    let paths: Vec<_> = std::fs::read_dir(containing)?
        .filter_map(|dir_item| {
            let Ok(dir_item) = dir_item else {
                return None;
            };

            let Some(str_name) = dir_item.file_name().to_str() else {
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

        let key = key
            .trim()
            .trim_start_matches(|p| p.to_digit(10).unwrap() == 0);

        key.parse::<u32>().unwrap_or_default()
    });

    Ok(paths)
}

fn determine_dataset_info(paths: &[PathBuf]) -> Result<()> {
    let image = image::open(paths.get(0).context("obtaining first image")?)?;

    let x = image.width();
    let y = image.height();
    let z: u32 = paths.len().try_into()?;

    match image.color() {
        image::ColorType::L8 => {
            let image = image.as_luma8().context("reading l8")?;

            image.enumerate_rows()
        }
        image::ColorType::L16 => {}
        _ => bail!("image must have a single channel only"),
    }
}
