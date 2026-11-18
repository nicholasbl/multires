use crate::*;

pub(crate) fn import_from_amrex(resolved: &Resolved) -> Result<(MsgDataset, String)> {
    let plotfile = PlotFile::open(&resolved.input)
        .with_context(|| format!("opening AMReX plotfile {}", resolved.input.display()))?;
    let configured = resolved
        .config
        .source
        .fields
        .iter()
        .map(|field| (field.name(), field.precision()))
        .collect::<Vec<_>>();
    let requested = if resolved.cli_fields.is_empty() {
        configured
    } else {
        resolved
            .cli_fields
            .iter()
            .map(|name| (name.as_str(), ConfigScalarType::F32))
            .collect()
    };
    let mut fields = Vec::with_capacity(requested.len());
    for (index, (name, scalar_type)) in requested.iter().enumerate() {
        ensure!(
            !requested[..index]
                .iter()
                .any(|(earlier, _)| earlier == name),
            "AMReX variable {name:?} was requested more than once"
        );
        let variable = plotfile
            .variable(name)
            .with_context(|| format!("AMReX variable {name:?} was not found"))?;
        fields.push(AmrexField {
            component: variable.index,
            scalar_type: (*scalar_type).into(),
            normalization: configured_normalization(&resolved.config, name)?,
        });
    }
    let replace_invalid = resolved.cli_replace_invalid
        || resolved
            .config
            .source
            .amrex
            .as_ref()
            .is_some_and(|config| config.invalid_values == InvalidValueConfig::Zero);
    let (dataset, report) = import_amrex(
        &plotfile,
        &AmrexImportOptions {
            fields,
            invalid_value_policy: if replace_invalid {
                InvalidValuePolicy::ReplaceWithZero
            } else {
                InvalidValuePolicy::Reject
            },
        },
    )?;
    for (field, replacements) in dataset.fields().iter().zip(&report.replaced_values) {
        if *replacements != 0 {
            eprintln!(
                "warning: replaced {replacements} invalid values with zero in {:?}",
                field.name()
            );
        }
    }
    Ok((
        dataset,
        format!(
            "AMReX import: {:.6}s, patches {}, samples {}",
            report.elapsed.as_secs_f64(),
            report.patch_count,
            report.sample_count
        ),
    ))
}
