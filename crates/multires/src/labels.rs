//! Exact label selection and exposed-voxel boundary meshing.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{Context, Result, ensure};
use glam::{IVec3, UVec3};

use crate::{
    Mesh3D, MsgDataset, MsgScalarGrid,
    sparse::{Aabb3u, SparseGrid3},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelMeshOptions {
    pub field: String,
    pub labels: Vec<u16>,
    pub level: usize,
}

/// Select the union of exact labels. Missing samples are never selected.
pub fn select_labels(values: &MsgScalarGrid, labels: &[u16]) -> Result<SparseGrid3<()>> {
    ensure!(
        values.scalar_type().is_label(),
        "label selection requires a label field"
    );
    ensure!(!labels.is_empty(), "at least one label must be selected");
    let selected = labels.iter().copied().collect::<HashSet<_>>();
    ensure!(
        selected.len() == labels.len(),
        "selected labels contain duplicates"
    );
    if matches!(values, MsgScalarGrid::LabelU8(_)) {
        ensure!(
            labels.iter().all(|&value| value <= u16::from(u8::MAX)),
            "u8 label selection contains a value above 255"
        );
    }

    let mut mask = SparseGrid3::new(values.bounds());
    values.for_each_value_in_aabb(Aabb3u::full_grid(values.bounds()), |position, value| {
        if selected.contains(&(value as u16)) {
            mask.set(position, ());
        }
    });
    Ok(mask)
}

/// Mesh the boundary of the union of selected labels.
///
/// Unselected labels, missing samples, and positions beyond the level bounds
/// are outside. Winding points from selected voxels toward outside.
pub fn mesh_labels(dataset: &MsgDataset, options: &LabelMeshOptions) -> Result<Mesh3D> {
    let field = dataset
        .field(&options.field)
        .with_context(|| format!("label field {:?} is absent", options.field))?;
    ensure!(
        field.scalar_type().is_label(),
        "field {:?} is continuous, not labelled",
        options.field
    );
    let level = dataset
        .levels()
        .get(options.level)
        .context("label mesh level is out of bounds")?;
    let values = field
        .levels()
        .get(options.level)
        .context("label field level is absent")?
        .values();
    let mask = select_labels(values, &options.labels)?;
    mesh_label_mask(&mask, dataset.geometry(), level.index_origin, level.scale)
}

pub fn mesh_label_mask(
    mask: &SparseGrid3<()>,
    geometry: crate::MsgGeometry,
    index_origin: IVec3,
    level_scale: glam::DVec3,
) -> Result<Mesh3D> {
    let mut faces = Vec::new();
    mask.for_each_present_in_aabb(mask.bounds_aabb(), |voxel, ()| {
        for (offset, corners) in face_definitions(voxel) {
            let neighbor = voxel.as_ivec3() + offset;
            let neighbor_inside = neighbor.cmpge(IVec3::ZERO).all()
                && neighbor.cmplt(mask.bounds().as_ivec3()).all()
                && mask.get(neighbor.as_uvec3()).is_some();
            if !neighbor_inside {
                faces.push((voxel, corners));
            }
        }
    });

    let mut mesh = Mesh3D::default();
    let mut vertices = HashMap::<([u32; 3], [u32; 3]), u32>::new();
    for (voxel, corners) in faces {
        let mut face = [0_u32; 4];
        for (slot, lattice) in corners.into_iter().enumerate() {
            let component = incident_component(mask, lattice, voxel);
            let key = (lattice.to_array(), component.to_array());
            face[slot] = if let Some(&id) = vertices.get(&key) {
                id
            } else {
                let id = u32::try_from(mesh.positions.len())
                    .context("label mesh exceeds u32 vertices")?;
                let local = lattice.as_dvec3() - glam::DVec3::splat(0.5);
                let position = geometry
                    .point_position_to_world(index_origin, level_scale, local)
                    .as_vec3()
                    .to_array();
                mesh.positions.push(position);
                vertices.insert(key, id);
                id
            };
        }
        mesh.indices.push([face[0], face[1], face[2]]);
        mesh.indices.push([face[0], face[2], face[3]]);
    }
    Ok(mesh)
}

fn incident_component(mask: &SparseGrid3<()>, lattice: UVec3, source: UVec3) -> UVec3 {
    let mut queue = VecDeque::from([source]);
    let mut visited = HashSet::from([source]);
    let mut representative = source;
    while let Some(voxel) = queue.pop_front() {
        if (voxel.z, voxel.y, voxel.x) < (representative.z, representative.y, representative.x) {
            representative = voxel;
        }
        for offset in [
            IVec3::X,
            -IVec3::X,
            IVec3::Y,
            -IVec3::Y,
            IVec3::Z,
            -IVec3::Z,
        ] {
            let candidate = voxel.as_ivec3() + offset;
            if candidate.cmpge(IVec3::ZERO).all() && candidate.cmplt(mask.bounds().as_ivec3()).all()
            {
                let candidate = candidate.as_uvec3();
                let incident = (0..3).all(|axis| {
                    let c = candidate.to_array()[axis];
                    let v = lattice.to_array()[axis];
                    c == v || c.checked_add(1) == Some(v)
                });
                if incident && mask.get(candidate).is_some() && visited.insert(candidate) {
                    queue.push_back(candidate);
                }
            }
        }
    }
    representative
}

fn face_definitions(voxel: UVec3) -> [(IVec3, [UVec3; 4]); 6] {
    let p = voxel;
    [
        (
            -IVec3::X,
            [p, p + UVec3::Z, p + UVec3::Y + UVec3::Z, p + UVec3::Y],
        ),
        (
            IVec3::X,
            [
                p + UVec3::X,
                p + UVec3::X + UVec3::Y,
                p + UVec3::ONE,
                p + UVec3::X + UVec3::Z,
            ],
        ),
        (
            -IVec3::Y,
            [p, p + UVec3::X, p + UVec3::X + UVec3::Z, p + UVec3::Z],
        ),
        (
            IVec3::Y,
            [
                p + UVec3::Y,
                p + UVec3::Y + UVec3::Z,
                p + UVec3::ONE,
                p + UVec3::X + UVec3::Y,
            ],
        ),
        (
            -IVec3::Z,
            [p, p + UVec3::Y, p + UVec3::X + UVec3::Y, p + UVec3::X],
        ),
        (
            IVec3::Z,
            [
                p + UVec3::Z,
                p + UVec3::X + UVec3::Z,
                p + UVec3::ONE,
                p + UVec3::Y + UVec3::Z,
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{DMat4, DVec3};

    #[test]
    fn union_omits_internal_label_faces() -> Result<()> {
        let mut values = MsgScalarGrid::new(UVec3::new(2, 1, 1), crate::MsgScalarType::LabelU8);
        values
            .write_u8_values(Aabb3u::full_grid(values.bounds()), &[1, 2])
            .map_err(|error| anyhow::anyhow!("writing labels: {error:?}"))?;
        let mask = select_labels(&values, &[1, 2])?;
        let mesh = mesh_label_mask(
            &mask,
            crate::cell_center_to_point_geometry(DMat4::IDENTITY)?,
            IVec3::ZERO,
            DVec3::ONE,
        )?;
        assert_eq!(mesh.indices.len(), 20);
        Ok(())
    }

    #[test]
    fn corner_contacts_keep_distinct_vertices() -> Result<()> {
        let mut mask = SparseGrid3::new(UVec3::splat(2));
        mask.set(UVec3::ZERO, ());
        mask.set(UVec3::ONE, ());
        let mesh = mesh_label_mask(
            &mask,
            crate::cell_center_to_point_geometry(DMat4::IDENTITY)?,
            IVec3::ZERO,
            DVec3::ONE,
        )?;
        assert_eq!(mesh.positions.len(), 16);
        Ok(())
    }
}
