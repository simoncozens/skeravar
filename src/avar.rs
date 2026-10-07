use font_types::{F2Dot14, Fixed};
use skrifa::raw::{tables::avar::SegmentMaps, ReadError, TopLevelTable};
use write_fonts::read::tables::{avar::Avar, variations::DeltaSetIndex};

use crate::{
    serialize::SerializeErrorFlags,
    variations::solver::{renormalize_value, Triple, TripleDistances},
    Subset, SubsetError,
};

impl Subset for Avar<'_> {
    fn subset(
        &self,
        plan: &crate::Plan,
        _font: &write_fonts::read::FontRef,
        s: &mut crate::serialize::Serializer,
        _builder: &mut write_fonts::FontBuilder,
    ) -> Result<(), crate::SubsetError> {
        if plan.axes_index_map.is_empty() {
            return Err(SubsetError::SubsetTableError(Avar::TAG)); // empty
        }
        subset_avar(self, plan, s).map_err(|_| SubsetError::SubsetTableError(Avar::TAG))
    }
}

fn subset_avar(
    avar: &Avar<'_>,
    plan: &crate::Plan,
    s: &mut crate::serialize::Serializer,
) -> Result<(), SerializeErrorFlags> {
    let new_axis_count = plan.axes_index_map.len() as u16;

    // Version
    s.embed(1_u16)?;
    s.embed(0_u16)?;
    s.embed(0_u16)?; // reserved
    s.embed(new_axis_count)?;

    for (i, segment_map) in avar.axis_segment_maps().iter().enumerate() {
        let Ok(segment_map) = segment_map else {
            return Err(SerializeErrorFlags::SERIALIZE_ERROR_READ_ERROR);
        };
        if plan.axes_index_map.contains_key(&i) {
            let Some(axis_tag) = plan.axes_old_index_tag_map.get(&i) else {
                return Err(SerializeErrorFlags::SERIALIZE_ERROR_OTHER);
            };
            // Subset the mapping
            if let Some(axis_range) = plan.axes_location.get(axis_tag) {
                let Some(&triple_distances) = plan.axes_triple_distances.get(axis_tag) else {
                    continue;
                };
                let unmapped_range: Triple = unmap_axis_range(axis_range, &segment_map);
                let axis_range =
                    Triple::new(axis_range.minimum, axis_range.middle, axis_range.maximum);
                let triple_distances =
                    TripleDistances::new(triple_distances.negative, triple_distances.positive);
                let mut value_mappings = vec![];
                for mapping in segment_map.axis_value_maps() {
                    let mapping_from = mapping.from_coordinate().to_f32() as f64;
                    if !unmapped_range.contains(mapping_from) {
                        continue;
                    }
                    let mapping_to = mapping.to_coordinate().to_f32() as f64;
                    let new_mapping = (
                        renormalize_value(mapping_from, unmapped_range, triple_distances, false),
                        renormalize_value(mapping_to, axis_range, triple_distances, false),
                    );
                    if must_include(new_mapping) {
                        continue;
                    }
                    value_mappings.push(new_mapping);
                }
                value_mappings.push((-1.0, -1.0));
                value_mappings.push((0.0, 0.0));
                value_mappings.push((1.0, 1.0));
                value_mappings.sort_by_key(|(from, _)| F2Dot14::from_f32(*from as f32).to_bits());
                s.embed(value_mappings.len() as u16)?;
                for (from, to) in value_mappings {
                    s.embed(F2Dot14::from_f32(from as f32))?;
                    s.embed(F2Dot14::from_f32(to as f32))?;
                }
            } else {
                // Just embed it as-is
                s.embed(segment_map.position_map_count())?;
                for mapping in segment_map.axis_value_maps() {
                    s.embed(mapping.from_coordinate())?;
                    s.embed(mapping.to_coordinate())?;
                }
            }
        }
    }
    Ok(())
}

/// Applies the `avar` mapping to a set of normalized coordinates (in 2.14 units).
///
/// Mirrors HarfBuzz's `OT::avar::map_coords_2_14`. For version 1 fonts this is
/// just the per-axis segment maps. For avar version 2 fonts the segment-mapped
/// coordinates additionally have the delta from the axis index map + item
/// variation store applied; that delta is evaluated using the *segment-mapped*
/// coordinates, exactly as HarfBuzz does.
pub(crate) fn map_coords_2_14(avar: &Avar, coords: Vec<f32>) -> Result<Vec<f32>, ReadError> {
    let mut coords = coords;
    let axis_count = avar.axis_count() as usize;
    let count = coords.len().min(axis_count);

    // Segment-mapped coordinates as raw 2.14 integers.
    let mut coords_2_14 = vec![0i32; coords.len()];
    for (i, maybe_map) in avar.axis_segment_maps().iter().take(count).enumerate() {
        let segment_map = maybe_map?;
        let v = (segment_map
            .apply(Fixed::from_f64(coords[i] as f64))
            .to_f32()
            * 16384.0)
            .round() as i32;
        coords_2_14[i] = v;
        coords[i] = v as f32 / 16384.0;
    }

    if avar.version().major < 2 {
        return Ok(coords);
    }

    // avar2: apply the (axis index -> variation index) map and evaluate the
    // item variation store at the segment-mapped coordinates.
    let varidx_map = avar.axis_index_map().transpose()?;
    let var_store = avar.var_store().transpose()?;

    let mapped_coords: Vec<F2Dot14> = coords_2_14
        .iter()
        .map(|&v| F2Dot14::from_bits(v as i16))
        .collect();

    for (i, coord) in coords.iter_mut().enumerate() {
        let varidx = match &varidx_map {
            Some(map) => map.get(i as u32)?,
            None => DeltaSetIndex {
                outer: (i >> 16) as u16,
                inner: i as u16,
            },
        };
        let delta = match &var_store {
            Some(store) => store
                .compute_delta(varidx, &mapped_coords)
                .map(|d| d.to_i32())
                .unwrap_or(0),
            None => 0,
        };
        // Apply the delta unclamped and clamp only the result to [-1, +1],
        // matching HarfBuzz/fontTools.
        let mut v = coords_2_14[i] + delta.clamp(-(1 << 15), 1 << 15);
        v = v.clamp(-(1 << 14), 1 << 14);
        *coord = v as f32 / 16384.0;
    }

    Ok(coords)
}

fn unmap_axis_range(range: &Triple, segment_maps: &SegmentMaps) -> Triple {
    Triple {
        minimum: unmap_float(range.minimum, segment_maps),
        middle: unmap_float(range.middle, segment_maps),
        maximum: unmap_float(range.maximum, segment_maps),
    }
}

enum Direction {
    #[allow(dead_code)]
    Forward,
    Backward,
}

fn unmap_float(f: f64, segment_maps: &SegmentMaps) -> f64 {
    map_float(f, Direction::Backward, segment_maps)
}

fn map_float(value: f64, direction: Direction, segment_maps: &SegmentMaps) -> f64 {
    let maps = segment_maps.axis_value_maps();
    let len = maps.len();
    if len < 2 {
        if len == 0 {
            return value;
        }
        let from_coord = maps[0].from_coordinate().to_f32() as f64;
        let to_coord = maps[0].to_coordinate().to_f32() as f64;
        return value - from_coord + to_coord;
    }

    let get_from_coord_val = |index: usize| match direction {
        Direction::Forward => maps[index].from_coordinate().to_bits() as f64 / 16384.0,
        Direction::Backward => maps[index].to_coordinate().to_bits() as f64 / 16384.0,
    };
    let get_to_coord_val = |index: usize| match direction {
        Direction::Forward => maps[index].to_coordinate().to_bits() as f64 / 16384.0,
        Direction::Backward => maps[index].from_coordinate().to_bits() as f64 / 16384.0,
    };

    let mut start = 0usize;
    let mut end = len;
    if get_from_coord_val(start) == -1.0
        && get_to_coord_val(start) == -1.0
        && get_from_coord_val(start + 1) == -1.0
    {
        start += 1;
    }
    if get_from_coord_val(end - 1) == 1.0
        && get_to_coord_val(end - 1) == 1.0
        && get_from_coord_val(end - 2) == 1.0
    {
        end -= 1;
    }

    let mut i = start;
    while i < end {
        if value == get_from_coord_val(i) {
            break;
        }
        i += 1;
    }
    if i < end {
        let mut j = i;
        while j + 1 < end {
            if value != get_from_coord_val(j + 1) {
                break;
            }
            j += 1;
        }

        if i == j {
            return get_to_coord_val(i);
        }
        if i + 2 == j {
            return get_to_coord_val(i + 1);
        }

        if value < 0.0 {
            return get_to_coord_val(j);
        }
        if value > 0.0 {
            return get_to_coord_val(i);
        }

        return if get_to_coord_val(i).abs() < get_to_coord_val(j).abs() {
            get_to_coord_val(i)
        } else {
            get_to_coord_val(j)
        };
    }

    let mut i = start;
    while i < end {
        if value < get_from_coord_val(i) {
            break;
        }
        i += 1;
    }

    if i == 0 {
        return value - get_from_coord_val(0) + get_to_coord_val(0);
    }
    if i == end {
        return value - get_from_coord_val(end - 1) + get_to_coord_val(end - 1);
    }

    let before = i - 1;
    let after = i;
    let denom = get_from_coord_val(after) - get_from_coord_val(before);
    get_to_coord_val(before)
        + ((get_to_coord_val(after) - get_to_coord_val(before))
            * (value - get_from_coord_val(before)))
            / denom
}

const F_EPSILON: f64 = 0.00001; // Epsilon for float comparison

fn float_approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < F_EPSILON
}

fn must_include(mapping: (f64, f64)) -> bool {
    // Only check for f64, as this is where the `new_mapping` values come from
    let neg_one = -1.0;
    let zero = 0.0;
    let one = 1.0;

    let map_from = mapping.0;
    let map_to = mapping.1;

    (float_approx_eq(map_from, neg_one) && float_approx_eq(map_to, neg_one))
        || (float_approx_eq(map_from, zero) && float_approx_eq(map_to, zero))
        || (float_approx_eq(map_from, one) && float_approx_eq(map_to, one))
}

#[cfg(test)]
mod tests {
    use skrifa::{
        raw::{collections::IntSet, TableProvider},
        FontRef,
    };

    use crate::{parse_instancing_spec, subset_font, Plan, SubsetFlags};

    #[test]
    fn test_variation_subspace() {
        let font = FontRef::new(include_bytes!("../test-data/fonts/NotoSans-VF.abc.ttf")).unwrap();
        let spec = parse_instancing_spec("wght=400:700,wdth=drop,CTGR=drop").unwrap();

        let plan = Plan::new(
            &IntSet::all(),
            &IntSet::all(),
            &font,
            SubsetFlags::default(),
            &IntSet::empty(),
            &IntSet::all(),
            &IntSet::all(),
            &IntSet::all(),
            &IntSet::all(),
            &Some(spec),
        );
        let newfont = subset_font(&font, &plan).unwrap();
        std::fs::write("newfont.ttf", &newfont).unwrap();
        let newfontref = FontRef::new(&newfont).unwrap();
        let avar = newfontref.avar().unwrap();
        let mappings = avar.axis_segment_maps();
        assert_eq!(mappings.iter().count(), 1);
        let mapping = mappings.iter().next().unwrap().unwrap();
        assert_eq!(mapping.position_map_count(), 5);
        let mut axis_value_maps = mapping.axis_value_maps().iter();
        let m1 = axis_value_maps.next().unwrap();
        assert_eq!(m1.from_coordinate().to_f32(), -1.0);
        assert_eq!(m1.to_coordinate().to_f32(), -1.0);
        let m2 = axis_value_maps.next().unwrap();
        assert_eq!(m2.from_coordinate().to_f32(), 0.0);
        assert_eq!(m2.to_coordinate().to_f32(), 0.0);
        let m3 = axis_value_maps.next().unwrap();
        assert_eq!(m3.from_coordinate().to_f32(), 0.33337402);
        assert_eq!(m3.to_coordinate().to_f32(), 0.29510498);
        let m4 = axis_value_maps.next().unwrap();
        assert_eq!(m4.from_coordinate().to_f32(), 0.66674805);
        assert_eq!(m4.to_coordinate().to_f32(), 0.62298584);
        let m5 = axis_value_maps.next().unwrap();
        assert_eq!(m5.from_coordinate().to_f32(), 1.0);
        assert_eq!(m5.to_coordinate().to_f32(), 1.0);
    }
}
