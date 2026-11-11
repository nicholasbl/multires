use std::{
    borrow::Cow,
    collections::HashSet,
    error::Error,
    path::Path,
    str::FromStr,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use glam::{DMat4, DVec3, IVec3, UVec3};
use multires_rs::{
    MsgDataset, MsgField, MsgFieldLevel, MsgGeometry, MsgLevel, MsgScalarGrid, MsgScalarType,
    sparse::Aabb3u,
};
use rayon::prelude::*;
use roxmltree::Node;

/// VTI importer policy.
///
/// Version 0.1 imports inline-base64 and raw-appended, uncompressed `Float32`
/// and `Float64` PointData.
/// Multi-component arrays are decomposed into scalar fields with component
/// suffixes such as `_x`, `_y`, and `_z`.
#[derive(Debug, Clone, Default)]
pub struct VtiImportOptions {
    /// Exact PointData array names to import. Empty means all supported arrays.
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct VtiImportTimings {
    pub mmap: Duration,
    pub parse_utf8: Duration,
    pub parse_document: Duration,
}

struct Timer(Instant);

impl Timer {
    fn new() -> Self {
        Self(Instant::now())
    }

    fn timestamp(&mut self) -> Duration {
        let elapsed = self.0.elapsed();
        self.0 = Instant::now();
        elapsed
    }
}

/// Read one VTK ImageData (`.vti`) timestep into an owned MSG dataset.
pub fn import_vti(
    path: impl AsRef<Path>,
    options: &VtiImportOptions,
) -> Result<(MsgDataset, VtiImportTimings)> {
    let path = path.as_ref();
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening VTI file {}", path.display()))?;
    let mut timer = Timer::new();

    // SAFETY: The mapping is read-only and remains alive for the XML document's lifetime.
    let mmap = unsafe { memmap2::Mmap::map(&file).context("memory-mapping VTI XML")? };
    let mmap_time = timer.timestamp();
    let raw_appended = raw_appended_source(&mmap, false)?;
    let (dataset, utf8_time, parse_time) = if let Some(raw) = raw_appended {
        let utf8_time = timer.timestamp();
        let document = roxmltree::Document::parse(&raw.metadata)
            .context("parsing raw-appended VTI metadata")?;
        let parse_time = timer.timestamp();
        let dataset = parse_root(document.root_element(), options, Some(raw.data))?;
        (dataset, utf8_time, parse_time)
    } else {
        match std::str::from_utf8(&mmap) {
            Ok(source) => {
                let utf8_time = timer.timestamp();
                let document = roxmltree::Document::parse(source).context("parsing VTI XML")?;
                let parse_time = timer.timestamp();
                let dataset = parse_root(document.root_element(), options, None)?;
                (dataset, utf8_time, parse_time)
            }
            Err(utf8_error) => {
                let raw = raw_appended_source(&mmap, true)?
                    .with_context(|| format!("VTI XML is not UTF-8: {utf8_error}"))?;
                let utf8_time = timer.timestamp();
                let document = roxmltree::Document::parse(&raw.metadata)
                    .context("parsing raw-appended VTI metadata")?;
                let parse_time = timer.timestamp();
                let dataset = parse_root(document.root_element(), options, Some(raw.data))?;
                (dataset, utf8_time, parse_time)
            }
        }
    };
    Ok((
        dataset,
        VtiImportTimings {
            mmap: mmap_time,
            parse_utf8: utf8_time,
            parse_document: parse_time,
        },
    ))
}

struct RawAppendedSource<'a> {
    metadata: String,
    data: &'a [u8],
}

fn raw_appended_source(
    source: &[u8],
    search_entire_file: bool,
) -> Result<Option<RawAppendedSource<'_>>> {
    const PREFIX_SCAN_LIMIT: usize = 16 * 1024 * 1024;
    let search_end = if search_entire_file {
        source.len()
    } else {
        source.len().min(PREFIX_SCAN_LIMIT)
    };
    let Some(tag_start) = memchr::memmem::find(&source[..search_end], b"<AppendedData") else {
        return Ok(None);
    };
    let relative_tag_end = memchr::memchr(b'>', &source[tag_start..])
        .context("VTI AppendedData opening tag is not terminated")?;
    let tag_end = tag_start + relative_tag_end;
    let opening_tag = std::str::from_utf8(&source[tag_start..=tag_end])
        .context("VTI AppendedData opening tag is not UTF-8")?;
    let wrapper = format!("<Root>{opening_tag}</AppendedData></Root>");
    let tag_document =
        roxmltree::Document::parse(&wrapper).context("parsing VTI AppendedData opening tag")?;
    let appended_node = tag_document
        .root_element()
        .first_element_child()
        .context("reading VTI AppendedData opening tag")?;
    if appended_node.attribute("encoding") != Some("raw") {
        return Ok(None);
    }

    let marker = source[tag_end + 1..]
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .map(|offset| tag_end + 1 + offset)
        .context("raw VTI AppendedData has no '_' marker")?;
    ensure!(
        source[marker] == b'_',
        "raw VTI AppendedData does not begin with the required '_' marker"
    );

    // Raw appended bytes can violate XML and UTF-8. Parse only the metadata
    // prefix, closing the already-open elements synthetically.
    let metadata_prefix = std::str::from_utf8(&source[..=tag_end])
        .context("raw-appended VTI metadata is not UTF-8")?;
    let mut metadata = String::with_capacity(metadata_prefix.len() + 32);
    metadata.push_str(metadata_prefix);
    metadata.push_str("</AppendedData></VTKFile>");
    Ok(Some(RawAppendedSource {
        metadata,
        data: &source[marker + 1..],
    }))
}

#[cfg(test)]
fn parse_vti_bytes(source: &[u8], options: &VtiImportOptions) -> Result<MsgDataset> {
    if let Some(raw) = raw_appended_source(source, false)? {
        let document = roxmltree::Document::parse(&raw.metadata)?;
        parse_root(document.root_element(), options, Some(raw.data))
    } else {
        match std::str::from_utf8(source) {
            Ok(source) => {
                let document = roxmltree::Document::parse(source)?;
                parse_root(document.root_element(), options, None)
            }
            Err(utf8_error) => {
                let raw = raw_appended_source(source, true)?
                    .with_context(|| format!("VTI XML is not UTF-8: {utf8_error}"))?;
                let document = roxmltree::Document::parse(&raw.metadata)?;
                parse_root(document.root_element(), options, Some(raw.data))
            }
        }
    }
}

#[derive(Clone, Copy)]
enum HeaderType {
    U32,
    U64,
}

impl HeaderType {
    fn size(self) -> usize {
        match self {
            Self::U32 => 4,
            Self::U64 => 8,
        }
    }

    fn payload_size(self, bytes: &[u8]) -> Result<usize> {
        let value = match self {
            Self::U32 => u64::from(u32::from_le_bytes(bytes.try_into()?)),
            Self::U64 => u64::from_le_bytes(bytes.try_into()?),
        };
        usize::try_from(value).context("VTI binary payload does not fit in memory")
    }
}

impl FromStr for HeaderType {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "UInt32" => Ok(Self::U32),
            "UInt64" => Ok(Self::U64),
            other => bail!("unsupported VTI header_type {other:?}"),
        }
    }
}

struct ParseContext<'a> {
    header: HeaderType,
    appended: Option<&'a [u8]>,
}

fn parse_root(
    node: Node<'_, '_>,
    options: &VtiImportOptions,
    appended: Option<&[u8]>,
) -> Result<MsgDataset> {
    ensure!(
        node.has_tag_name("VTKFile"),
        "VTI root element is not VTKFile"
    );
    check_required_attribute(node, "type", "ImageData")?;
    check_required_attribute(node, "byte_order", "LittleEndian")?;
    ensure!(
        node.attribute("compressor").is_none(),
        "compressed VTI data is not supported"
    );

    let header = node.attribute("header_type").unwrap_or("UInt32").parse()?;
    let image_data = node
        .children()
        .find(|child| child.has_tag_name("ImageData"))
        .context("VTI file has no ImageData element")?;
    parse_image_data(image_data, ParseContext { header, appended }, options)
}

fn parse_image_data(
    node: Node<'_, '_>,
    ctx: ParseContext<'_>,
    options: &VtiImportOptions,
) -> Result<MsgDataset> {
    let whole_extent = parse_extent(required_attribute(node, "WholeExtent")?)?;
    let point_extent = extent_size(whole_extent)?;
    let point_count = point_count(point_extent)?;
    let origin = parse_fixed_list::<f64, 3>(required_attribute(node, "Origin")?)?;
    let spacing = parse_fixed_list::<f64, 3>(required_attribute(node, "Spacing")?)?;
    let direction = node
        .attribute("Direction")
        .map(parse_fixed_list::<f64, 9>)
        .transpose()?
        .unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);

    let pieces = node
        .children()
        .filter(|child| child.has_tag_name("Piece"))
        .collect::<Vec<_>>();
    ensure!(!pieces.is_empty(), "VTI ImageData has no Piece element");
    ensure!(
        pieces.len() == 1,
        "multi-piece VTI files are not supported (found {} pieces)",
        pieces.len()
    );
    let piece_extent = parse_extent(required_attribute(pieces[0], "Extent")?)?;
    ensure!(
        piece_extent == whole_extent,
        "the VTI Piece extent does not match WholeExtent"
    );

    let geometry = MsgGeometry::point_centered(grid_to_world(origin, direction))
        .context("building MSG geometry from VTI Origin and Direction")?;
    let level = MsgLevel {
        index_origin: IVec3::new(whole_extent[0], whole_extent[2], whole_extent[4]),
        point_extent,
        scale: DVec3::from_array(spacing),
        refinement_ratio_from_coarse: None,
    };
    let time = parse_time(node, &ctx)?.unwrap_or(0.0);
    let mut dataset =
        MsgDataset::new(time, geometry, vec![level]).context("building MSG dataset metadata")?;

    let requested = options
        .fields
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    ensure!(
        requested.len() == options.fields.len(),
        "VTI field selection contains duplicate names"
    );
    let mut found = HashSet::new();

    for point_data in pieces[0]
        .children()
        .filter(|child| child.has_tag_name("PointData"))
    {
        for array_node in point_data
            .children()
            .filter(|child| child.has_tag_name("DataArray"))
        {
            let name = required_attribute(array_node, "Name")?;
            if !requested.is_empty() && !requested.contains(name) {
                continue;
            }
            found.insert(name.to_owned());
            let array = parse_data_array(array_node, &ctx, Some(point_count))
                .with_context(|| format!("reading VTI PointData array {name:?}"))?;
            for field in convert_data_array(array, point_extent)? {
                dataset
                    .add_field(field)
                    .context("adding decomposed VTI field to MSG dataset")?;
            }
        }
    }

    let missing = requested
        .iter()
        .filter(|name| !found.contains(**name))
        .copied()
        .collect::<Vec<_>>();
    ensure!(
        missing.is_empty(),
        "requested VTI fields were not found: {missing:?}"
    );
    Ok(dataset)
}

fn parse_time(node: Node<'_, '_>, ctx: &ParseContext<'_>) -> Result<Option<f64>> {
    for field_data in node
        .children()
        .filter(|child| child.has_tag_name("FieldData"))
    {
        for array_node in field_data
            .children()
            .filter(|child| child.has_tag_name("DataArray"))
        {
            if array_node.attribute("Name") != Some("TimeValue") {
                continue;
            }
            let array =
                parse_data_array(array_node, ctx, Some(1)).context("reading VTI TimeValue")?;
            ensure!(array.component_count == 1, "VTI TimeValue is not scalar");
            let value = match array.ty {
                DataArrayType::F32 => f64::from(array.f32_values()?[0]),
                DataArrayType::F64 => array.f64_values()?[0],
            };
            ensure!(value.is_finite(), "VTI TimeValue is not finite");
            return Ok(Some(value));
        }
    }
    Ok(None)
}

#[derive(Clone, Copy)]
enum DataArrayType {
    F32,
    F64,
}

impl DataArrayType {
    fn size(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F64 => 8,
        }
    }

    fn scalar_type(self) -> MsgScalarType {
        match self {
            Self::F32 => MsgScalarType::F32,
            Self::F64 => MsgScalarType::F64,
        }
    }
}

enum DataArrayPayload<'a> {
    Inline {
        storage: Vec<u64>,
        payload_start: usize,
        payload_len: usize,
    },
    Appended(&'a [u8]),
}

impl DataArrayPayload<'_> {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Inline {
                storage,
                payload_start,
                payload_len,
            } => &bytemuck::cast_slice(storage)[*payload_start..*payload_start + *payload_len],
            Self::Appended(bytes) => bytes,
        }
    }
}

struct DataArray<'a> {
    name: String,
    ty: DataArrayType,
    component_count: usize,
    payload: DataArrayPayload<'a>,
}

impl DataArray<'_> {
    fn payload(&self) -> &[u8] {
        self.payload.bytes()
    }

    fn f32_values(&self) -> Result<Cow<'_, [f32]>> {
        if cfg!(target_endian = "little")
            && let Ok(values) = bytemuck::try_cast_slice(self.payload())
        {
            return Ok(Cow::Borrowed(values));
        }
        Ok(Cow::Owned(
            self.payload()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f32::from_le_bytes(*bytes))
                .collect(),
        ))
    }

    fn f64_values(&self) -> Result<Cow<'_, [f64]>> {
        if cfg!(target_endian = "little")
            && let Ok(values) = bytemuck::try_cast_slice(self.payload())
        {
            return Ok(Cow::Borrowed(values));
        }
        Ok(Cow::Owned(
            self.payload()
                .as_chunks::<8>()
                .0
                .iter()
                .map(|bytes| f64::from_le_bytes(*bytes))
                .collect(),
        ))
    }
}

fn parse_data_array<'a>(
    node: Node<'_, '_>,
    ctx: &ParseContext<'a>,
    expected_tuple_count: Option<usize>,
) -> Result<DataArray<'a>> {
    let name = required_attribute(node, "Name")?.to_owned();
    let ty = match required_attribute(node, "type")? {
        "Float32" => DataArrayType::F32,
        "Float64" => DataArrayType::F64,
        other => bail!("unsupported VTI DataArray type {other:?}"),
    };
    let component_count = node
        .attribute("NumberOfComponents")
        .unwrap_or("1")
        .parse::<usize>()
        .context("parsing VTI NumberOfComponents")?;
    ensure!(
        (1..=4).contains(&component_count),
        "VTI arrays must have between one and four components"
    );
    let expected_len = expected_tuple_count
        .map(|tuple_count| {
            tuple_count
                .checked_mul(component_count)
                .and_then(|count| count.checked_mul(ty.size()))
                .context("VTI DataArray dimensions overflow usize")
        })
        .transpose()?;
    let payload = match required_attribute(node, "format")? {
        "binary" => decode_inline_payload(node, ctx.header)?,
        "appended" => {
            let appended = ctx
                .appended
                .context("VTI appended arrays require raw AppendedData")?;
            let offset = required_attribute(node, "offset")?
                .parse::<usize>()
                .context("parsing VTI appended DataArray offset")?;
            appended_payload(appended, offset, ctx.header)?
        }
        other => bail!("unsupported VTI DataArray format {other:?}"),
    };
    let actual_len = payload.bytes().len();
    ensure!(
        actual_len.is_multiple_of(ty.size()),
        "VTI payload byte count is not divisible by its scalar size"
    );
    if let Some(expected_len) = expected_len {
        ensure!(
            actual_len == expected_len,
            "VTI DataArray has {actual_len} payload bytes; expected {expected_len}"
        );
    }

    Ok(DataArray {
        name,
        ty,
        component_count,
        payload,
    })
}

fn decode_inline_payload(
    node: Node<'_, '_>,
    header: HeaderType,
) -> Result<DataArrayPayload<'static>> {
    let encoded = node
        .text()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .context("VTI DataArray has no binary content")?;
    // Decode into u64-aligned storage. Padding before a UInt32 header places
    // both Float32 and Float64 payloads on a safe alignment boundary.
    let header_size = header.size();
    let prefix_padding = (8 - header_size) % 8;
    let decoded_capacity = base64::decoded_len_estimate(encoded.len());
    let storage_bytes = prefix_padding
        .checked_add(decoded_capacity)
        .context("VTI DataArray is too large")?;
    let word_count = storage_bytes
        .checked_add(7)
        .context("VTI DataArray is too large")?
        / 8;
    let mut storage = vec![0_u64; word_count];
    let bytes = bytemuck::cast_slice_mut::<u64, u8>(&mut storage);
    let decoded_len = BASE64_STANDARD
        .decode_slice(encoded, &mut bytes[prefix_padding..])
        .context("decoding VTI base64 DataArray")?;
    ensure!(
        decoded_len >= header_size,
        "VTI binary payload has no size header"
    );
    let payload_start = prefix_padding + header_size;
    let declared_len = header.payload_size(&bytes[prefix_padding..payload_start])?;
    let actual_len = decoded_len - header_size;
    ensure!(
        declared_len == actual_len,
        "VTI payload header declares {declared_len} bytes but {actual_len} bytes follow"
    );
    Ok(DataArrayPayload::Inline {
        storage,
        payload_start,
        payload_len: actual_len,
    })
}

fn appended_payload(
    appended: &[u8],
    offset: usize,
    header: HeaderType,
) -> Result<DataArrayPayload<'_>> {
    let header_end = offset
        .checked_add(header.size())
        .context("VTI appended DataArray offset overflow")?;
    let header_bytes = appended
        .get(offset..header_end)
        .context("VTI appended DataArray header is out of bounds")?;
    let payload_len = header.payload_size(header_bytes)?;
    let payload_end = header_end
        .checked_add(payload_len)
        .context("VTI appended DataArray size overflow")?;
    let payload = appended
        .get(header_end..payload_end)
        .context("VTI appended DataArray payload is out of bounds")?;
    Ok(DataArrayPayload::Appended(payload))
}

fn convert_data_array(array: DataArray<'_>, extent: UVec3) -> Result<Vec<MsgField>> {
    let suffixes = ["x", "y", "z", "w"];
    match array.ty {
        DataArrayType::F32 => {
            let values = array.f32_values()?;
            (0..array.component_count)
                .into_par_iter()
                .map(|component| {
                    let name =
                        component_name(&array.name, array.component_count, component, suffixes);
                    let level = if array.component_count == 1 {
                        field_level_f32(&values, extent)?
                    } else {
                        let component_values = values
                            .par_iter()
                            .skip(component)
                            .step_by(array.component_count)
                            .copied()
                            .collect::<Vec<_>>();
                        field_level_f32(&component_values, extent)?
                    };
                    Ok(MsgField::new(name, array.ty.scalar_type(), vec![level]))
                })
                .collect()
        }
        DataArrayType::F64 => {
            let values = array.f64_values()?;
            (0..array.component_count)
                .into_par_iter()
                .map(|component| {
                    let name =
                        component_name(&array.name, array.component_count, component, suffixes);
                    let level = if array.component_count == 1 {
                        field_level_f64(&values, extent)?
                    } else {
                        let component_values = values
                            .par_iter()
                            .skip(component)
                            .step_by(array.component_count)
                            .copied()
                            .collect::<Vec<_>>();
                        field_level_f64(&component_values, extent)?
                    };
                    Ok(MsgField::new(name, array.ty.scalar_type(), vec![level]))
                })
                .collect()
        }
    }
}

fn component_name(
    base: &str,
    component_count: usize,
    component: usize,
    suffixes: [&str; 4],
) -> String {
    if component_count == 1 {
        base.to_owned()
    } else {
        format!("{base}_{}", suffixes[component])
    }
}

fn field_level_f32(values: &[f32], extent: UVec3) -> Result<MsgFieldLevel> {
    let mut grid = MsgScalarGrid::new(extent, MsgScalarType::F32);
    let non_finite_count = values.par_iter().filter(|value| !value.is_finite()).count();
    if non_finite_count == 0 {
        write_f32_values(&mut grid, Aabb3u::full_grid(extent), values)?;
    } else if non_finite_count <= sparse_clear_threshold(values.len()) {
        write_f32_values(&mut grid, Aabb3u::full_grid(extent), values)?;
        let missing = values
            .par_iter()
            .enumerate()
            .filter_map(|(index, value)| (!value.is_finite()).then_some(index))
            .collect::<Vec<_>>();
        for index in missing {
            grid.clear(linear_point(index, extent));
        }
    } else {
        write_finite_spans_f32(&mut grid, values, extent)?;
    }
    Ok(MsgFieldLevel::new(grid))
}

fn field_level_f64(values: &[f64], extent: UVec3) -> Result<MsgFieldLevel> {
    let mut grid = MsgScalarGrid::new(extent, MsgScalarType::F64);
    let non_finite_count = values.par_iter().filter(|value| !value.is_finite()).count();
    if non_finite_count == 0 {
        write_f64_values(&mut grid, Aabb3u::full_grid(extent), values)?;
    } else if non_finite_count <= sparse_clear_threshold(values.len()) {
        write_f64_values(&mut grid, Aabb3u::full_grid(extent), values)?;
        let missing = values
            .par_iter()
            .enumerate()
            .filter_map(|(index, value)| (!value.is_finite()).then_some(index))
            .collect::<Vec<_>>();
        for index in missing {
            grid.clear(linear_point(index, extent));
        }
    } else {
        write_finite_spans_f64(&mut grid, values, extent)?;
    }
    Ok(MsgFieldLevel::new(grid))
}

// Sparse clear is cheap when missing samples are exceptional. Above this
// density, writing only finite x-runs avoids allocating chunks for absent data.
fn sparse_clear_threshold(value_count: usize) -> usize {
    value_count / 16
}

#[derive(Debug)]
struct FiniteSpan {
    range: std::ops::Range<usize>,
    aabb: Aabb3u,
}

fn finite_spans<T: Sync>(
    values: &[T],
    extent: UVec3,
    is_finite: impl Fn(&T) -> bool + Sync,
) -> Vec<FiniteSpan> {
    let nx = extent.x as usize;
    let ny = extent.y as usize;
    values
        .par_chunks(nx)
        .enumerate()
        .flat_map_iter(|(row_index, row)| {
            let y = (row_index % ny) as u32;
            let z = (row_index / ny) as u32;
            let row_start = row_index * nx;
            let mut spans = Vec::new();
            let mut x = 0;
            while x < row.len() {
                while x < row.len() && !is_finite(&row[x]) {
                    x += 1;
                }
                let start = x;
                while x < row.len() && is_finite(&row[x]) {
                    x += 1;
                }
                if start < x {
                    spans.push(FiniteSpan {
                        range: row_start + start..row_start + x,
                        aabb: Aabb3u::new(
                            UVec3::new(start as u32, y, z),
                            UVec3::new(x as u32, y + 1, z + 1),
                        ),
                    });
                }
            }
            spans
        })
        .collect()
}

fn write_finite_spans_f32(grid: &mut MsgScalarGrid, values: &[f32], extent: UVec3) -> Result<()> {
    for span in finite_spans(values, extent, |value| value.is_finite()) {
        write_f32_values(grid, span.aabb, &values[span.range])?;
    }
    Ok(())
}

fn write_finite_spans_f64(grid: &mut MsgScalarGrid, values: &[f64], extent: UVec3) -> Result<()> {
    for span in finite_spans(values, extent, |value| value.is_finite()) {
        write_f64_values(grid, span.aabb, &values[span.range])?;
    }
    Ok(())
}

fn write_f32_values(grid: &mut MsgScalarGrid, aabb: Aabb3u, values: &[f32]) -> Result<()> {
    grid.write_f32_values(aabb, values)
        .map_err(|error| anyhow::anyhow!("writing Float32 values to MSG grid: {error:?}"))
}

fn write_f64_values(grid: &mut MsgScalarGrid, aabb: Aabb3u, values: &[f64]) -> Result<()> {
    grid.write_f64_values(aabb, values)
        .map_err(|error| anyhow::anyhow!("writing Float64 values to MSG grid: {error:?}"))
}

fn linear_point(index: usize, extent: UVec3) -> UVec3 {
    let nx = extent.x as usize;
    let ny = extent.y as usize;
    let z = index / (nx * ny);
    let remainder = index % (nx * ny);
    let y = remainder / nx;
    let x = remainder % nx;
    UVec3::new(x as u32, y as u32, z as u32)
}

fn parse_extent(value: &str) -> Result<[i32; 6]> {
    parse_fixed_list(value)
}

fn extent_size(extent: [i32; 6]) -> Result<UVec3> {
    fn axis(min: i32, max: i32) -> Result<u32> {
        ensure!(max >= min, "VTI extent maximum is less than its minimum");
        let size = i64::from(max) - i64::from(min) + 1;
        u32::try_from(size).context("VTI extent is too large")
    }
    Ok(UVec3::new(
        axis(extent[0], extent[1])?,
        axis(extent[2], extent[3])?,
        axis(extent[4], extent[5])?,
    ))
}

fn point_count(extent: UVec3) -> Result<usize> {
    (extent.x as usize)
        .checked_mul(extent.y as usize)
        .and_then(|count| count.checked_mul(extent.z as usize))
        .context("VTI point count overflows usize")
}

fn grid_to_world(origin: [f64; 3], direction: [f64; 9]) -> DMat4 {
    // VTK serializes Direction in row-major order; glam takes column-major columns.
    DMat4::from_cols_array(&[
        direction[0],
        direction[3],
        direction[6],
        0.0,
        direction[1],
        direction[4],
        direction[7],
        0.0,
        direction[2],
        direction[5],
        direction[8],
        0.0,
        origin[0],
        origin[1],
        origin[2],
        1.0,
    ])
}

fn parse_fixed_list<T, const N: usize>(value: &str) -> Result<[T; N]>
where
    T: FromStr,
    T::Err: Send + Sync + Error + 'static,
{
    let values = value
        .split_whitespace()
        .map(str::parse)
        .collect::<std::result::Result<Vec<T>, _>>()
        .context("parsing VTI numeric list")?;
    values.try_into().map_err(|values: Vec<T>| {
        anyhow::anyhow!(
            "VTI numeric list has {} entries; expected {N}",
            values.len()
        )
    })
}

fn required_attribute<'a>(node: Node<'a, '_>, name: &str) -> Result<&'a str> {
    node.attribute(name)
        .with_context(|| format!("missing required VTI attribute {name:?}"))
}

fn check_required_attribute(node: Node<'_, '_>, name: &str, expected: &str) -> Result<()> {
    let actual = required_attribute(node, name)?;
    ensure!(
        actual == expected,
        "VTI attribute {name:?} is {actual:?}; expected {expected:?}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binary_f32(values: &[f32]) -> String {
        let mut bytes = Vec::with_capacity(8 + values.len() * 4);
        bytes.extend_from_slice(&(values.len() as u64 * 4).to_le_bytes());
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        BASE64_STANDARD.encode(bytes)
    }

    fn binary_f64(values: &[f64]) -> String {
        let mut bytes = Vec::with_capacity(8 + values.len() * 8);
        bytes.extend_from_slice(&(values.len() as u64 * 8).to_le_bytes());
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        BASE64_STANDARD.encode(bytes)
    }

    fn document(point_arrays: &str, field_data: &str) -> roxmltree::Document<'static> {
        let xml = format!(
            r#"<VTKFile type="ImageData" byte_order="LittleEndian" header_type="UInt64">
              <ImageData WholeExtent="-1 0 2 2 0 0" Origin="10 20 30" Spacing="2 3 4"
                         Direction="0 -1 0 1 0 0 0 0 1">
                {field_data}
                <Piece Extent="-1 0 2 2 0 0"><PointData>{point_arrays}</PointData></Piece>
              </ImageData>
            </VTKFile>"#
        );
        roxmltree::Document::parse(Box::leak(xml.into_boxed_str())).unwrap()
    }

    #[test]
    fn imports_geometry_and_decomposes_vectors() {
        let vector = binary_f32(&[1.0, 2.0, f32::NAN, 4.0]);
        let document = document(
            &format!(
                r#"<DataArray type="Float32" Name="velocity" NumberOfComponents="2" format="binary">{vector}</DataArray>"#
            ),
            "",
        );
        let dataset =
            parse_root(document.root_element(), &VtiImportOptions::default(), None).unwrap();

        assert_eq!(dataset.time(), 0.0);
        assert_eq!(dataset.levels()[0].index_origin, IVec3::new(-1, 2, 0));
        assert_eq!(dataset.levels()[0].point_extent, UVec3::new(2, 1, 1));
        assert_eq!(dataset.levels()[0].scale, DVec3::new(2.0, 3.0, 4.0));
        assert_eq!(
            dataset.geometry().point_to_world(
                dataset.levels()[0].index_origin,
                dataset.levels()[0].scale,
                UVec3::ZERO,
            ),
            DVec3::new(4.0, 18.0, 30.0)
        );
        assert_eq!(
            dataset
                .fields()
                .iter()
                .map(MsgField::name)
                .collect::<Vec<_>>(),
            ["velocity_x", "velocity_y"]
        );
        assert_eq!(
            dataset.field("velocity_x").unwrap().levels()[0]
                .values()
                .get(UVec3::ZERO),
            Some(1.0)
        );
        assert_eq!(
            dataset.field("velocity_x").unwrap().levels()[0]
                .values()
                .get(UVec3::X),
            None
        );
        assert_eq!(
            dataset.field("velocity_y").unwrap().levels()[0]
                .values()
                .get(UVec3::X),
            Some(4.0)
        );
    }

    #[test]
    fn reads_time_and_filters_arrays() {
        let keep = binary_f64(&[5.0, 6.0]);
        let skip = binary_f32(&[7.0, 8.0]);
        let time = binary_f64(&[2.05]);
        let arrays = format!(
            r#"<DataArray type="Float64" Name="keep" format="binary">{keep}</DataArray>
               <DataArray type="Float32" Name="skip" format="binary">{skip}</DataArray>"#
        );
        let field_data = format!(
            r#"<FieldData><DataArray type="Float64" Name="TimeValue" format="binary">{time}</DataArray></FieldData>"#
        );
        let document = document(&arrays, &field_data);
        let dataset = parse_root(
            document.root_element(),
            &VtiImportOptions {
                fields: vec!["keep".into()],
            },
            None,
        )
        .unwrap();
        assert_eq!(dataset.time(), 2.05);
        assert_eq!(
            dataset
                .fields()
                .iter()
                .map(MsgField::name)
                .collect::<Vec<_>>(),
            ["keep"]
        );
    }

    #[test]
    fn bulk_writes_then_clears_rare_non_finite_values() {
        let extent = UVec3::new(64, 1, 1);
        let mut values = (0..64).map(|value| value as f32).collect::<Vec<_>>();
        values[37] = f32::INFINITY;

        let level = field_level_f32(&values, extent).unwrap();
        assert_eq!(level.values().present_point_count(), 63);
        assert_eq!(level.values().get(UVec3::new(36, 0, 0)), Some(36.0));
        assert_eq!(level.values().get(UVec3::new(37, 0, 0)), None);
        assert_eq!(level.values().get(UVec3::new(38, 0, 0)), Some(38.0));
    }

    #[test]
    fn imports_raw_appended_arrays_by_offset() {
        let metadata =
            br#"<VTKFile type="ImageData" byte_order="LittleEndian" header_type="UInt64">
          <ImageData WholeExtent="0 1 0 0 0 0" Origin="1 2 3" Spacing="1 1 1">
            <FieldData>
              <DataArray type="Float64" Name="TimeValue" format="appended" offset="0"/>
            </FieldData>
            <Piece Extent="0 1 0 0 0 0">
              <PointData>
                <DataArray type="Float32" Name="velocity" NumberOfComponents="2"
                           format="appended" offset="16"/>
              </PointData>
            </Piece>
          </ImageData>
          <AppendedData encoding="raw">_"#;
        let mut source = metadata.to_vec();
        source.extend_from_slice(&8_u64.to_le_bytes());
        source.extend_from_slice(&2.5_f64.to_le_bytes());
        source.extend_from_slice(&16_u64.to_le_bytes());
        for value in [1.0_f32, 2.0, f32::from_bits(0x7fff_ffff), 4.0] {
            source.extend_from_slice(&value.to_le_bytes());
        }
        source.extend_from_slice(b"</AppendedData></VTKFile>");

        let dataset = parse_vti_bytes(&source, &VtiImportOptions::default()).unwrap();
        assert_eq!(dataset.time(), 2.5);
        assert_eq!(
            dataset
                .fields()
                .iter()
                .map(MsgField::name)
                .collect::<Vec<_>>(),
            ["velocity_x", "velocity_y"]
        );
        assert_eq!(
            dataset.field("velocity_x").unwrap().levels()[0]
                .values()
                .get(UVec3::ZERO),
            Some(1.0)
        );
        assert_eq!(
            dataset.field("velocity_x").unwrap().levels()[0]
                .values()
                .get(UVec3::X),
            None
        );
        assert_eq!(
            dataset.field("velocity_y").unwrap().levels()[0]
                .values()
                .get(UVec3::X),
            Some(4.0)
        );
    }

    #[test]
    fn rejects_mismatched_payload_length() {
        let values = binary_f32(&[1.0]);
        let document = document(
            &format!(
                r#"<DataArray type="Float32" Name="short" format="binary">{values}</DataArray>"#
            ),
            "",
        );
        let error =
            parse_root(document.root_element(), &VtiImportOptions::default(), None).unwrap_err();
        assert!(error.to_string().contains("PointData array"));
        assert!(format!("{error:#}").contains("expected 8"));
    }

    #[test]
    fn rejects_non_image_vtk_root() {
        let document = roxmltree::Document::parse(
            r#"<VTKFile type="PolyData" byte_order="LittleEndian"><PolyData/></VTKFile>"#,
        )
        .unwrap();
        let error =
            parse_root(document.root_element(), &VtiImportOptions::default(), None).unwrap_err();
        assert!(error.to_string().contains("type"));
    }

    #[test]
    #[ignore = "set MSG_VTI_TEST_FILE to run against a large external fixture"]
    fn imports_external_vti_fixture() {
        let path = std::env::var("MSG_VTI_TEST_FILE").expect("MSG_VTI_TEST_FILE is not set");
        let fields = std::env::var("MSG_VTI_TEST_FIELDS")
            .ok()
            .map(|names| names.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        let (dataset, _) = import_vti(path, &VtiImportOptions { fields }).unwrap();
        dataset.validate().unwrap();
        assert!(!dataset.fields().is_empty());
    }
}
